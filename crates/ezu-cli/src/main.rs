//! Command-line renderer for the Ezu Style Spec.
//!
//! ```text
//! ezu tile --style STYLE.json --pmtiles URL_OR_PATH --tile Z/X/Y [--out FILE]
//! ezu bbox --style STYLE.json --mvt-url 'https://.../{z}/{x}/{y}.pbf' \
//!          --bbox MIN_LNG,MIN_LAT,MAX_LNG,MAX_LAT --zoom Z [--out FILE]
//! ```

mod serve;
pub(crate) mod source;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Args, Parser, Subcommand, ValueEnum};
use ezu::core::TileId as CoreTileId;
use ezu::features::mvt;
use ezu::graph::{
    build_graph, Cache, CanvasInfo, Evaluator, Graph, ParamValues, PortValue, RasterBuf, TileId,
};
use ezu::paint::host::{
    pixmap_to_webp, raster_to_png, raster_to_webp, BrushBankLoader, TileLoader,
};
use ezu::paint::nodes::default_registry;
use ezu::style::{AssetKind, Document};
use futures::future::try_join_all;
use futures::stream::{StreamExt, TryStreamExt};
use tiny_skia::{Pixmap, PixmapPaint, Transform};
use tracing_subscriber::EnvFilter;

use crate::source::{SourceSpec, TileSource};

#[derive(Parser, Debug)]
#[command(name = "ezu", about = "Render Ezu Style documents to PNG")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Render a single z/x/y tile to PNG.
    Tile(TileCmd),
    /// Render the tile mosaic covering a lon/lat bounding box at a fixed zoom.
    Bbox(BboxCmd),
    /// Bulk-render an XYZ tile pyramid into `<out>/<z>/<x>/<y>.png`.
    Tiles(TilesCmd),
    /// Validate an Ezu Style document without rendering — exits non-zero
    /// on parse / graph / asset errors. Suitable for CI + pre-commit hooks.
    Check(CheckCmd),
    /// Start the live editor + tile server at `http://127.0.0.1:8080`.
    Serve(serve::ServeCmd),
}

#[derive(Args, Debug)]
struct CommonArgs {
    /// Ezu Style JSON document — local path or http(s):// URL.
    #[arg(long)]
    style: String,
    /// Base directory for resolving asset `src` paths. Defaults to the
    /// style file's parent directory (or the current directory when
    /// `--style` is a URL).
    #[arg(long)]
    assets_dir: Option<PathBuf>,
    /// PMTiles archive — local path or http(s):// URL.
    #[arg(long, conflicts_with = "mvt")]
    pmtiles: Option<String>,
    /// Templated MVT tile source containing `{z}`, `{x}`, `{y}`
    /// placeholders. Accepts an http(s):// URL or a local path
    /// template (e.g. `/tiles/{z}/{x}/{y}.pbf`).
    #[arg(long, conflicts_with = "pmtiles")]
    mvt: Option<String>,
}

/// Output raster format. Pure-Rust pipelines on both sides — WebP is
/// lossless via the `image-webp` codec, typically 20–40 % smaller than
/// PNG for painterly content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Png,
    Webp,
}

impl OutputFormat {
    fn extension(self) -> &'static str {
        match self {
            OutputFormat::Png => "png",
            OutputFormat::Webp => "webp",
        }
    }

    /// Pick a format from a filename extension, falling back to PNG.
    fn from_path(path: &Path) -> Self {
        match path.extension().and_then(|s| s.to_str()) {
            Some(s) if s.eq_ignore_ascii_case("webp") => OutputFormat::Webp,
            _ => OutputFormat::Png,
        }
    }
}

#[derive(Args, Debug)]
struct CheckCmd {
    /// Ezu Style JSON document — local path or http(s):// URL.
    style: String,
    /// Base directory for resolving relative asset `src` paths.
    /// Defaults to the style file's parent directory (or the current
    /// directory when `--style` is a URL).
    #[arg(long)]
    assets_dir: Option<PathBuf>,
    /// Skip fetching URL assets and reading local asset files — only
    /// run parse + `build_graph`. Faster and works offline; misses
    /// errors like an unreachable brush URL or a missing image file.
    #[arg(long)]
    no_fetch: bool,
}

#[derive(Args, Debug)]
struct TileCmd {
    #[command(flatten)]
    common: CommonArgs,
    /// Tile coordinate as `Z/X/Y`.
    #[arg(long, value_parser = parse_zxy)]
    tile: CoreTileId,
    /// Output path. Format is sniffed from the extension (`.png` /
    /// `.webp`); use `--format` to override.
    #[arg(long, default_value = "out.png")]
    out: PathBuf,
    /// Output format. Defaults to whatever `--out`'s extension implies.
    #[arg(long, value_enum)]
    format: Option<OutputFormat>,
}

#[derive(Args, Debug)]
struct BboxCmd {
    #[command(flatten)]
    common: CommonArgs,
    /// Bounding box `min_lng,min_lat,max_lng,max_lat` (WGS84).
    #[arg(long, value_parser = parse_bbox)]
    bbox: BBox,
    /// Zoom level.
    #[arg(long)]
    zoom: u8,
    /// Output path. Format is sniffed from the extension (`.png` /
    /// `.webp`); use `--format` to override.
    #[arg(long, default_value = "out.png")]
    out: PathBuf,
    /// Output format. Defaults to whatever `--out`'s extension implies.
    #[arg(long, value_enum)]
    format: Option<OutputFormat>,
}

#[derive(Args, Debug)]
struct TilesCmd {
    #[command(flatten)]
    common: CommonArgs,
    /// Bounding box `min_lng,min_lat,max_lng,max_lat` (WGS84). When
    /// omitted, every tile at each zoom is generated — at z=14 that
    /// is 268M tiles, so a bbox is strongly recommended.
    #[arg(long, value_parser = parse_bbox)]
    bbox: Option<BBox>,
    /// Minimum zoom level (inclusive).
    #[arg(long)]
    min_zoom: u8,
    /// Maximum zoom level (inclusive).
    #[arg(long)]
    max_zoom: u8,
    /// Output directory; tiles are written as
    /// `<out>/<z>/<x>/<y>.<ext>` (extension picked by `--format`).
    #[arg(long, default_value = "tiles")]
    out: PathBuf,
    /// Output format. Defaults to PNG.
    #[arg(long, value_enum, default_value_t = OutputFormat::Png)]
    format: OutputFormat,
    /// Number of tiles rendered in parallel. Defaults to the number
    /// of logical CPU cores.
    #[arg(long, default_value_t = default_concurrency())]
    concurrency: usize,
}

fn default_concurrency() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
}

#[derive(Clone, Copy, Debug)]
struct BBox {
    min_lng: f64,
    min_lat: f64,
    max_lng: f64,
    max_lat: f64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Tile(args) => run_tile(args).await,
        Cmd::Bbox(args) => run_bbox(args).await,
        Cmd::Tiles(args) => run_tiles(args).await,
        Cmd::Check(args) => run_check(args).await,
        Cmd::Serve(args) => serve::run(args).await,
    }
}

/// Shared one-time setup: parse the style, build the graph, load
/// declared assets, and open the tile source. Returned components are
/// `Arc`-wrapped so callers can hand them to concurrent render tasks.
struct Prepared {
    graph: Arc<Graph>,
    cache: Arc<Cache>,
    loader: Arc<BrushBankLoader>,
    source: Arc<TileSource>,
    canvas: CanvasInfo,
}

async fn prepare(common: &CommonArgs) -> Result<Prepared, Box<dyn std::error::Error>> {
    let style_text = fetch_text(&common.style).await?;
    let doc = Document::from_json(&style_text)?;
    tracing::info!(
        "style: {} v{} ({} nodes, tile={}, pad={})",
        doc.name,
        doc.version,
        doc.nodes.len(),
        doc.tile_size,
        doc.pad,
    );

    let assets_dir = common.assets_dir.clone().unwrap_or_else(|| {
        if is_url(&common.style) {
            PathBuf::from(".")
        } else {
            Path::new(&common.style)
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."))
        }
    });
    let loader = Arc::new(build_asset_loader(&doc, &assets_dir).await?);

    let registry = default_registry();
    let graph = Arc::new(build_graph(&doc, &registry)?);
    let cache = Arc::new(Cache::new());
    let canvas = CanvasInfo {
        tile_size: doc.tile_size,
        pad: doc.pad,
    };

    let spec = match (&common.pmtiles, &common.mvt) {
        (Some(p), None) => SourceSpec::PmTiles(p.clone()),
        (None, Some(u)) => SourceSpec::Mvt(u.clone()),
        _ => return Err("one of --pmtiles or --mvt is required".into()),
    };
    tracing::info!("opening source: {spec:?}");
    let source = Arc::new(TileSource::open(&spec).await?);

    Ok(Prepared { graph, cache, loader, source, canvas })
}

async fn run_check(args: CheckCmd) -> Result<(), Box<dyn std::error::Error>> {
    let text = fetch_text(&args.style).await?;
    let doc = Document::from_json(&text)?;
    let registry = default_registry();
    let graph = build_graph(&doc, &registry)?;

    if !args.no_fetch && !doc.assets.is_empty() {
        let base_dir = args.assets_dir.clone().unwrap_or_else(|| {
            if is_url(&args.style) {
                PathBuf::from(".")
            } else {
                Path::new(&args.style)
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."))
            }
        });
        let mut loader = BrushBankLoader::new()
            .with_dir(base_dir.clone())
            .with_images_dir(base_dir.clone());
        ezu::paint::host::prefetch_doc_assets(&doc, &base_dir, &mut loader).await?;
    }

    tracing::info!(
        "ok: {} v{} ({} nodes, {} assets){}",
        doc.name,
        doc.version,
        graph.len(),
        doc.assets.len(),
        if args.no_fetch { " [parse + graph only]" } else { "" },
    );
    Ok(())
}

async fn run_tile(args: TileCmd) -> Result<(), Box<dyn std::error::Error>> {
    let prep = prepare(&args.common).await?;
    let format = args.format.unwrap_or_else(|| OutputFormat::from_path(&args.out));
    let raster = render_one(
        Arc::clone(&prep.graph),
        Arc::clone(&prep.cache),
        Arc::clone(&prep.loader),
        Arc::clone(&prep.source),
        prep.canvas,
        args.tile,
    )
    .await
    .map_err(|e| e.to_string())?;
    let bytes = match format {
        OutputFormat::Png => raster_to_png(&raster, prep.canvas.tile_size, prep.canvas.pad)?,
        OutputFormat::Webp => raster_to_webp(&raster, prep.canvas.tile_size, prep.canvas.pad)?,
    };
    std::fs::write(&args.out, &bytes)?;
    tracing::info!("wrote {} ({} bytes)", args.out.display(), bytes.len());
    Ok(())
}

async fn run_bbox(args: BboxCmd) -> Result<(), Box<dyn std::error::Error>> {
    let prep = prepare(&args.common).await?;
    let format = args.format.unwrap_or_else(|| OutputFormat::from_path(&args.out));
    let (x_range, y_range) = bbox_to_tiles(args.bbox, args.zoom);
    let nx = x_range.end - x_range.start;
    let ny = y_range.end - y_range.start;
    tracing::info!(
        "bbox covers {nx}×{ny} tiles at z={} ({}..{}, {}..{})",
        args.zoom,
        x_range.start,
        x_range.end,
        y_range.start,
        y_range.end,
    );

    let mut tasks = Vec::with_capacity((nx * ny) as usize);
    for ty in y_range.clone() {
        for tx in x_range.clone() {
            let tile = CoreTileId::new(args.zoom, tx, ty);
            let graph = Arc::clone(&prep.graph);
            let cache = Arc::clone(&prep.cache);
            let loader = Arc::clone(&prep.loader);
            let source = Arc::clone(&prep.source);
            let canvas = prep.canvas;
            tasks.push(tokio::spawn(async move {
                let raster = render_one(graph, cache, loader, source, canvas, tile).await?;
                Ok::<(CoreTileId, Arc<RasterBuf>), Box<dyn std::error::Error + Send + Sync>>((
                    tile, raster,
                ))
            }));
        }
    }

    let mut mosaic =
        Pixmap::new(nx * prep.canvas.tile_size, ny * prep.canvas.tile_size).ok_or("mosaic alloc")?;
    for handle in try_join_all(tasks).await? {
        let (tile, raster) = handle.map_err(|e| e.to_string())?;
        let dx = ((tile.x - x_range.start) * prep.canvas.tile_size) as i32;
        let dy = ((tile.y - y_range.start) * prep.canvas.tile_size) as i32;
        blit_padded_into(&mut mosaic, &raster, dx, dy, prep.canvas.tile_size, prep.canvas.pad)?;
    }
    let bytes = match format {
        OutputFormat::Png => mosaic.encode_png().map_err(|e| e.to_string())?,
        OutputFormat::Webp => pixmap_to_webp(&mosaic).map_err(|e| e.to_string())?,
    };
    std::fs::write(&args.out, &bytes)?;
    tracing::info!("wrote {} ({} bytes)", args.out.display(), bytes.len());
    Ok(())
}

async fn run_tiles(args: TilesCmd) -> Result<(), Box<dyn std::error::Error>> {
    if args.min_zoom > args.max_zoom {
        return Err("--min-zoom must be ≤ --max-zoom".into());
    }
    if args.concurrency == 0 {
        return Err("--concurrency must be ≥ 1".into());
    }
    let prep = prepare(&args.common).await?;

    let mut total: u64 = 0;
    for z in args.min_zoom..=args.max_zoom {
        let (x_range, y_range) = match args.bbox {
            Some(b) => bbox_to_tiles(b, z),
            None => {
                let n = 1u32 << z;
                (0..n, 0..n)
            }
        };
        let nx = x_range.end - x_range.start;
        let ny = y_range.end - y_range.start;
        let count = nx as u64 * ny as u64;
        total += count;
        tracing::info!(
            "z={z}: {nx}×{ny} = {count} tiles ({}..{}, {}..{})",
            x_range.start,
            x_range.end,
            y_range.start,
            y_range.end,
        );

        // Lazily enumerate every (x, y) so memory stays bounded even
        // when a whole-world zoom is requested.
        let xr = x_range.clone();
        let coords = y_range
            .clone()
            .flat_map(move |ty| xr.clone().map(move |tx| (tx, ty)));
        let prep = &prep;
        let out = &args.out;
        let format = args.format;
        let t0 = std::time::Instant::now();
        futures::stream::iter(coords)
            .map(|(tx, ty)| async move {
                let tile = CoreTileId::new(z, tx, ty);
                let raster = render_one(
                    Arc::clone(&prep.graph),
                    Arc::clone(&prep.cache),
                    Arc::clone(&prep.loader),
                    Arc::clone(&prep.source),
                    prep.canvas,
                    tile,
                )
                .await?;
                let bytes = tokio::task::spawn_blocking({
                    let canvas = prep.canvas;
                    move || match format {
                        OutputFormat::Png => raster_to_png(&raster, canvas.tile_size, canvas.pad),
                        OutputFormat::Webp => raster_to_webp(&raster, canvas.tile_size, canvas.pad),
                    }
                })
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.to_string().into() })?
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.to_string().into() })?;
                let dir = out.join(z.to_string()).join(tx.to_string());
                tokio::fs::create_dir_all(&dir).await?;
                let path = dir.join(format!("{ty}.{}", format.extension()));
                tokio::fs::write(&path, bytes).await?;
                Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
            })
            .buffer_unordered(args.concurrency)
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| e.to_string())?;
        tracing::info!(
            "z={z}: done in {:.1}s",
            t0.elapsed().as_secs_f64()
        );
    }
    tracing::info!("wrote {total} tiles → {}", args.out.display());
    Ok(())
}

async fn render_one(
    graph: Arc<Graph>,
    cache: Arc<Cache>,
    loader: Arc<BrushBankLoader>,
    source: Arc<TileSource>,
    canvas: CanvasInfo,
    tile: CoreTileId,
) -> Result<Arc<RasterBuf>, Box<dyn std::error::Error + Send + Sync>> {
    let mvt_bytes = source.fetch(tile).await?;
    let raster = tokio::task::spawn_blocking(move || -> Result<Arc<RasterBuf>, Box<dyn std::error::Error + Send + Sync>> {
        let tile_id = TileId { z: tile.z, x: tile.x, y: tile.y };
        let mut tile_loader = TileLoader::new(loader.as_ref(), tile_id);
        if let Some(bytes) = mvt_bytes {
            tile_loader.bind_mvt(mvt::decode(&bytes)?);
        }
        let ev = Evaluator::new(&graph, &cache, &tile_loader);
        let out = ev.render_parallel(tile_id, canvas, &ParamValues::new(), tile_seed(tile))?;
        match out {
            PortValue::Raster(r) => Ok(r),
            other => Err(format!("expected Raster output, got {:?}", other.kind()).into()),
        }
    })
    .await??;
    Ok(raster)
}

/// Pre-resolve every entry in the style's `assets` block: each `src`
/// may be a local file path (looked up against `base_dir`) or an
/// `http(s)://` URL (fetched via `ezu::paint::host::prefetch_doc_assets`).
/// Decoded payloads are staged in the returned [`BrushBankLoader`].
async fn build_asset_loader(
    doc: &Document,
    base_dir: &Path,
) -> Result<BrushBankLoader, Box<dyn std::error::Error>> {
    let mut loader = BrushBankLoader::new()
        .with_dir(base_dir.to_path_buf())
        .with_images_dir(base_dir.to_path_buf());
    ezu::paint::host::prefetch_doc_assets(doc, base_dir, &mut loader).await?;
    if doc
        .assets
        .values()
        .any(|d| d.kind == AssetKind::Gradient)
    {
        tracing::warn!("gradient assets are not yet supported by the CLI");
    }
    tracing::info!(
        "loaded {} brushes + {} images (base={})",
        loader.bank.len(),
        loader.images.len(),
        base_dir.display(),
    );
    Ok(loader)
}

/// Resolve `<base>/<src>` first as-is, then with `.<ext>` appended.
/// Mirrors `BrushBankLoader`'s lazy lookup so `assets.src` can omit
/// the extension as a shorthand (e.g. `"watercolor_glazing"` →
/// `"watercolor_glazing.myb"`).
/// Fetch a text resource by URL (http/https) or local path. Used for
/// the style document so callers can pass either form to `--style`.
pub(crate) async fn fetch_text(arg: &str) -> Result<String, Box<dyn std::error::Error>> {
    if is_url(arg) {
        let body = reqwest::get(arg).await?.error_for_status()?.text().await?;
        Ok(body)
    } else {
        Ok(std::fs::read_to_string(arg)?)
    }
}

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Copy the central `tile_size × tile_size` region of `raster` (which
/// is `pad`-padded on every side) into `mosaic` at `(dx, dy)`.
fn blit_padded_into(
    mosaic: &mut Pixmap,
    raster: &RasterBuf,
    dx: i32,
    dy: i32,
    tile_size: u32,
    pad: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    // Hand-crop the central tile region out of the padded raster so we
    // can paste with no clip-mask gymnastics. The mosaic gets exactly
    // the visible tile pixels and nothing of the neighbour-bleed pad.
    let mut tile = Pixmap::new(tile_size, tile_size).ok_or("tile pixmap alloc")?;
    let stride = (raster.width * 4) as usize;
    let row_bytes = (tile_size * 4) as usize;
    let dst = tile.data_mut();
    for row in 0..tile_size {
        let src_y = pad + row;
        let src_off = src_y as usize * stride + (pad as usize) * 4;
        let dst_off = row as usize * row_bytes;
        dst[dst_off..dst_off + row_bytes]
            .copy_from_slice(&raster.pixels[src_off..src_off + row_bytes]);
    }
    mosaic.draw_pixmap(
        dx,
        dy,
        tile.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    Ok(())
}

/// Convert a lon/lat bounding box to an inclusive-min, exclusive-max
/// tile range at the given zoom. Latitudes north of ~85.05° are
/// clamped to the Web Mercator domain.
fn bbox_to_tiles(b: BBox, z: u8) -> (std::ops::Range<u32>, std::ops::Range<u32>) {
    let n = 2f64.powi(z as i32);
    let xt = |lng: f64| ((lng + 180.0) / 360.0 * n).floor().clamp(0.0, n - 1.0) as u32;
    let yt = |lat: f64| {
        let lat = lat.clamp(-85.0511287798, 85.0511287798).to_radians();
        ((1.0 - (lat.tan().asinh() / std::f64::consts::PI)) / 2.0 * n)
            .floor()
            .clamp(0.0, n - 1.0) as u32
    };
    let x0 = xt(b.min_lng);
    let x1 = xt(b.max_lng);
    // min_lat is south → larger y_tile; max_lat is north → smaller y_tile.
    let y0 = yt(b.max_lat);
    let y1 = yt(b.min_lat);
    (x0..(x1 + 1), y0..(y1 + 1))
}

fn parse_zxy(s: &str) -> Result<CoreTileId, String> {
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() != 3 {
        return Err(format!("expected `Z/X/Y`, got `{s}`"));
    }
    let z: u8 = parts[0].parse().map_err(|e| format!("bad z: {e}"))?;
    let x: u32 = parts[1].parse().map_err(|e| format!("bad x: {e}"))?;
    let y: u32 = parts[2].parse().map_err(|e| format!("bad y: {e}"))?;
    Ok(CoreTileId::new(z, x, y))
}

fn parse_bbox(s: &str) -> Result<BBox, String> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 4 {
        return Err(format!("expected `min_lng,min_lat,max_lng,max_lat`, got `{s}`"));
    }
    let v: Vec<f64> = parts
        .iter()
        .map(|p| p.trim().parse::<f64>().map_err(|e| format!("bad number `{p}`: {e}")))
        .collect::<Result<_, _>>()?;
    let (min_lng, min_lat, max_lng, max_lat) = (v[0], v[1], v[2], v[3]);
    if min_lng >= max_lng || min_lat >= max_lat {
        return Err(format!(
            "bbox min must be strictly less than max: {min_lng},{min_lat},{max_lng},{max_lat}"
        ));
    }
    Ok(BBox { min_lng, min_lat, max_lng, max_lat })
}

fn tile_seed(tile: CoreTileId) -> u64 {
    let mut s = 0u64;
    s = s.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(tile.z as u64);
    s = s.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(tile.x as u64);
    s = s.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(tile.y as u64);
    s
}
