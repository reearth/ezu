//! Command-line renderer for the Ezu Style Spec.
//!
//! ```text
//! ezu tile --style STYLE.json --pmtiles URL_OR_PATH --tile Z/X/Y [--out FILE]
//! ezu bbox --style STYLE.json --mvt-url 'https://.../{z}/{x}/{y}.pbf' \
//!          --bbox MIN_LNG,MIN_LAT,MAX_LNG,MAX_LAT --zoom Z [--out FILE]
//! ```

mod source;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use ezu::core::TileId as CoreTileId;
use ezu::features::mvt;
use ezu::graph::{
    build_graph, Cache, CanvasInfo, Evaluator, Graph, ParamValues, PortValue, RasterBuf, TileId,
};
use ezu::paint::host::{raster_to_png, BrushBankLoader, TileLoader};
use ezu::paint::nodes::default_registry;
use ezu::style::{AssetKind, Document};
use futures::future::try_join_all;
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

#[derive(Args, Debug)]
struct TileCmd {
    #[command(flatten)]
    common: CommonArgs,
    /// Tile coordinate as `Z/X/Y`.
    #[arg(long, value_parser = parse_zxy)]
    tile: CoreTileId,
    /// Output PNG path.
    #[arg(long, default_value = "out.png")]
    out: PathBuf,
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
    /// Output PNG path.
    #[arg(long, default_value = "out.png")]
    out: PathBuf,
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
    let loader = Arc::new(build_asset_loader(&doc, &assets_dir)?);

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

async fn run_tile(args: TileCmd) -> Result<(), Box<dyn std::error::Error>> {
    let prep = prepare(&args.common).await?;
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
    let png = raster_to_png(&raster, prep.canvas.tile_size, prep.canvas.pad)?;
    std::fs::write(&args.out, &png)?;
    tracing::info!("wrote {} ({} bytes)", args.out.display(), png.len());
    Ok(())
}

async fn run_bbox(args: BboxCmd) -> Result<(), Box<dyn std::error::Error>> {
    let prep = prepare(&args.common).await?;
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
    let png = mosaic.encode_png().map_err(|e| e.to_string())?;
    std::fs::write(&args.out, &png)?;
    tracing::info!("wrote {} ({} bytes)", args.out.display(), png.len());
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

/// Pre-resolve every entry in the style's `assets` block against
/// `base_dir` and stage it in a `BrushBankLoader`. Brushes are parsed
/// from `.myb` JSON; image assets are decoded to premultiplied RGBA.
/// Gradient kinds are skipped with a warning.
fn build_asset_loader(
    doc: &Document,
    base_dir: &Path,
) -> Result<BrushBankLoader, Box<dyn std::error::Error>> {
    let mut loader = BrushBankLoader::new()
        .with_dir(base_dir.to_path_buf())
        .with_images_dir(base_dir.to_path_buf());
    for (name, decl) in &doc.assets {
        match decl.kind {
            AssetKind::Brush => {
                let path = resolve_with_ext(base_dir, &decl.src, "myb")
                    .ok_or_else(|| format!("brush asset `{name}`: no file at {}", base_dir.join(&decl.src).display()))?;
                let json = std::fs::read_to_string(&path)
                    .map_err(|e| format!("reading brush `{name}` at {}: {e}", path.display()))?;
                let brush = hokusai::myb::from_str(&json)
                    .map_err(|e| format!("parsing brush `{name}`: {e}"))?;
                loader.insert(name.clone(), brush);
            }
            AssetKind::Image | AssetKind::MaskImage => {
                let path = resolve_with_ext(base_dir, &decl.src, "png")
                    .ok_or_else(|| format!("image asset `{name}`: no file at {}", base_dir.join(&decl.src).display()))?;
                let raster = decode_image(&path)
                    .map_err(|e| format!("decoding image `{name}` at {}: {e}", path.display()))?;
                loader.insert_image(name.clone(), raster);
            }
            AssetKind::Gradient => {
                tracing::warn!("asset `{name}`: gradient assets are not yet supported by the CLI");
            }
        }
    }
    tracing::info!(
        "loaded {} brushes + {} images from {}",
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
async fn fetch_text(arg: &str) -> Result<String, Box<dyn std::error::Error>> {
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

fn resolve_with_ext(base: &Path, src: &str, ext: &str) -> Option<PathBuf> {
    [base.join(src), base.join(format!("{src}.{ext}"))]
        .into_iter()
        .find(|cand| cand.exists())
}

/// Decode an image file (PNG/JPEG/…) into a `RasterBuf` carrying
/// premultiplied RGBA8 — the storage convention used throughout
/// `ezu-graph` and `ezu-paint`.
fn decode_image(path: &Path) -> Result<RasterBuf, String> {
    let img = image::open(path).map_err(|e| e.to_string())?.to_rgba8();
    let (w, h) = img.dimensions();
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for px in img.pixels() {
        let [r, g, b, a] = px.0;
        let af = a as f32 / 255.0;
        pixels.push((r as f32 * af).round() as u8);
        pixels.push((g as f32 * af).round() as u8);
        pixels.push((b as f32 * af).round() as u8);
        pixels.push(a);
    }
    Ok(RasterBuf { width: w, height: h, pixels })
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
