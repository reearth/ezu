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
    bind_dem_sources, build_dem_sources, pixmap_to_webp, raster_to_png, raster_to_webp,
    BrushBankLoader, DemSourceRegistry, TileLoader,
};
use ezu::paint::nodes::default_registry;
use ezu::style::{Document, SourceDecl};
use futures::future::try_join_all;
use futures::stream::{StreamExt, TryStreamExt};
use tiny_skia::{Pixmap, PixmapPaint, Transform};
use tracing_subscriber::EnvFilter;

use crate::source::{SourceSpec, TileSource};

#[derive(Parser, Debug)]
#[command(name = "ezu", about = "Render Ezu Style documents to PNG")]
struct Cli {
    /// Emit per-node debug logs from the graph evaluator (op name,
    /// cache hit/miss, output shape, eval duration). Overrides
    /// `RUST_LOG` for this run.
    #[arg(long, short = 'v', global = true)]
    verbose: bool,
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
    /// Emit a Mermaid `graph LR` diagram of the style's node dependencies.
    Graph(GraphCmd),
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
    /// When a requested tile is missing, fall back to a parent tile
    /// up to this many zoom levels up and re-project its geometry
    /// onto the requested tile (MVT "overzoom"). `0` disables.
    #[arg(long, default_value_t = 4)]
    overzoom_levels: u8,
    /// Override a document parameter, as `name=value` (repeatable).
    /// Values are validated against the style's `params` declarations:
    /// numbers respect `min`/`max`, colors are `#rrggbb[aa]`, bools
    /// are `true`/`false`.
    #[arg(long = "param", value_name = "NAME=VALUE")]
    params: Vec<String>,
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
struct GraphCmd {
    /// Ezu Style JSON document — local path or http(s):// URL.
    style: String,
    /// Output file. Writes to stdout when omitted.
    #[arg(long)]
    out: Option<PathBuf>,
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
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
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
    let cli = Cli::parse();
    let filter = if cli.verbose {
        // Bump just the per-node evaluator target — info elsewhere keeps
        // the noise focused on the graph trace the user asked for.
        EnvFilter::new("info,ezu_graph::eval=debug")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();
    match cli.cmd {
        Cmd::Tile(args) => run_tile(args).await,
        Cmd::Bbox(args) => run_bbox(args).await,
        Cmd::Tiles(args) => run_tiles(args).await,
        Cmd::Check(args) => run_check(args).await,
        Cmd::Graph(args) => run_graph(args).await,
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
    source: Option<Arc<TileSource>>,
    /// Name of the document's mvt/pmtiles source that `source`
    /// resolves to; passed to `bind_mvt` so that bindings land under
    /// the same name the style's `features` nodes reference.
    source_name: Option<Arc<str>>,
    dem_sources: Arc<DemSourceRegistry>,
    canvas: CanvasInfo,
    overzoom_levels: u8,
    params: Arc<ParamValues>,
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

    // CLI flags override the URL but keep the doc's source NAME, since
    // the style's `features` nodes reference sources by name.
    let cli_override = match (&common.pmtiles, &common.mvt) {
        (Some(p), None) => Some((SourceSpec::PmTiles(p.clone()), "--pmtiles flag")),
        (None, Some(u)) => Some((SourceSpec::Mvt(u.clone()), "--mvt flag")),
        (None, None) => None,
        _ => return Err("--pmtiles and --mvt are mutually exclusive".into()),
    };
    let pick = feature_source_from_doc(&doc);
    let (source, source_name): (Option<Arc<TileSource>>, Option<Arc<str>>) = match (
        pick,
        cli_override,
    ) {
        (Some(p), Some((spec, origin))) => {
            tracing::info!("opening source ({origin}, bound as `{}`): {spec:?}", p.name);
            (
                Some(Arc::new(TileSource::open(&spec).await?)),
                Some(Arc::from(p.name)),
            )
        }
        (Some(p), None) => {
            tracing::info!("opening source ({}): {:?}", p.origin, p.spec);
            (
                Some(Arc::new(TileSource::open(&p.spec).await?)),
                Some(Arc::from(p.name)),
            )
        }
        (None, Some((spec, origin))) => {
            return Err(format!(
                    "{origin} ({spec:?}) requires the style to declare a matching `mvt`/`pmtiles` source, but the document has none — `features` nodes have no source to reference"
                )
                .into());
        }
        (None, None) => {
            tracing::info!("no MVT source — `features` bindings will be empty");
            (None, None)
        }
    };

    let dem_sources = Arc::new(build_dem_sources(&doc));
    if !dem_sources.is_empty() {
        let names: Vec<&str> = dem_sources.names().collect();
        tracing::info!("dem sources: {}", names.join(", "));
    }

    Ok(Prepared {
        graph,
        cache,
        loader,
        source,
        source_name,
        dem_sources,
        canvas,
        overzoom_levels: common.overzoom_levels,
        params: Arc::new(parse_cli_params(&common.params, &doc)?),
    })
}

/// Parse repeated `--param name=value` flags against the document's
/// `params` declarations. Unknown names, type mismatches, and
/// out-of-range numbers are hard errors.
fn parse_cli_params(
    flags: &[String],
    doc: &Document,
) -> Result<ParamValues, Box<dyn std::error::Error>> {
    let mut values = ParamValues::new();
    for flag in flags {
        let (name, raw) = flag
            .split_once('=')
            .ok_or_else(|| format!("--param `{flag}`: expected `name=value`"))?;
        let v = ezu::graph::parse_param_value(&doc.params, name, raw)?;
        values.set(name.to_string(), v);
    }
    Ok(values)
}

async fn run_check(args: CheckCmd) -> Result<(), Box<dyn std::error::Error>> {
    let text = fetch_text(&args.style).await?;
    let doc = Document::from_json(&text)?;
    let registry = default_registry();
    let graph = build_graph(&doc, &registry)?;

    let doc_scoped_count = count_doc_scoped_sources(&doc);
    if !args.no_fetch && doc_scoped_count > 0 {
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
        "ok: {} v{} ({} nodes, {} sources){}",
        doc.name,
        doc.version,
        graph.len(),
        doc.sources.len(),
        if args.no_fetch {
            " [parse + graph only]"
        } else {
            ""
        },
    );
    Ok(())
}

/// Number of document-scoped sources (brush / image) — the ones
/// `prefetch_doc_assets` will fetch on style load.
fn count_doc_scoped_sources(doc: &Document) -> usize {
    doc.sources
        .values()
        .filter(|d| matches!(d, SourceDecl::Brush(_) | SourceDecl::Image(_)))
        .count()
}

async fn run_graph(args: GraphCmd) -> Result<(), Box<dyn std::error::Error>> {
    let text = fetch_text(&args.style).await?;
    let doc = Document::from_json(&text)?;
    let mermaid = render_mermaid(&doc);
    match &args.out {
        Some(p) => {
            std::fs::write(p, &mermaid)?;
            tracing::info!("wrote {} ({} bytes)", p.display(), mermaid.len());
        }
        None => print!("{mermaid}"),
    }
    Ok(())
}

/// Render the document's node DAG as a Mermaid `graph LR` block.
/// Edges follow data flow: each `@ref` becomes `ref --> consumer`.
/// `sources` entries are emitted as styled nodes so brush/image and
/// tile-pyramid sources stay visible.
fn render_mermaid(doc: &ezu::style::Document) -> String {
    use std::collections::HashSet;

    let mut s = String::new();
    s.push_str("graph LR\n");

    let mut doc_scoped_ids: Vec<&str> = Vec::new();
    for (id, decl) in &doc.sources {
        let kind = match decl {
            SourceDecl::Brush(_) => "brush",
            SourceDecl::Image(_) => "image",
            SourceDecl::Mvt(_) => "mvt",
            SourceDecl::Pmtiles(_) => "pmtiles",
            SourceDecl::Dem(_) => "dem",
        };
        s.push_str(&format!("  {id}[/\"{id} (source:{kind})\"/]\n"));
        if matches!(decl, SourceDecl::Brush(_) | SourceDecl::Image(_)) {
            doc_scoped_ids.push(id);
        }
    }

    let output_id = doc.output.as_str();
    let mut source_ids: Vec<&str> = Vec::new();
    for (id, spec) in &doc.nodes {
        let is_source = spec.op == "features";
        let suffix = if id == output_id { ":::output" } else { "" };
        // Function calls label as `func:<name>` so the diagram reads at
        // the source level (calls stay single nodes, not expansions).
        let op = if spec.op == "func" {
            match spec.fields.get("fn").and_then(serde_json::Value::as_str) {
                Some(f) => format!("func:{f}"),
                None => spec.op.clone(),
            }
        } else {
            spec.op.clone()
        };
        // Cylinder shape for data-source nodes (MVT-backed `features`);
        // rectangle for everything else.
        if is_source {
            s.push_str(&format!("  {id}[(\"{id} ({op})\")]{suffix}\n"));
            source_ids.push(id);
        } else {
            s.push_str(&format!("  {id}[\"{id} ({op})\"]{suffix}\n"));
        }
    }
    s.push_str("  __output__([\"OUTPUT\"]):::sink\n");
    s.push_str(&format!("  {output_id} ==> __output__\n"));

    s.push('\n');
    for (id, spec) in &doc.nodes {
        let mut refs: Vec<String> = Vec::new();
        collect_refs(&serde_json::Value::Object(spec.fields.clone()), &mut refs);
        let mut seen = HashSet::new();
        for r in refs {
            if !seen.insert(r.clone()) {
                continue;
            }
            if doc.nodes.contains_key(&r) || doc.sources.contains_key(&r) {
                s.push_str(&format!("  {r} --> {id}\n"));
            }
        }
    }

    s.push_str("\n  classDef asset fill:#fff4d6,stroke:#a88500;\n");
    s.push_str("  classDef output fill:#ffe0e0,stroke:#cc3333,stroke-width:2px;\n");
    s.push_str("  classDef sink fill:#cc3333,color:#ffffff,stroke:#7a1f1f,stroke-width:2px;\n");
    s.push_str("  classDef source fill:#d9ecff,stroke:#2a6fb0;\n");
    if !doc_scoped_ids.is_empty() {
        s.push_str(&format!("  class {} asset;\n", doc_scoped_ids.join(",")));
    }
    if !source_ids.is_empty() {
        s.push_str(&format!("  class {} source;\n", source_ids.join(",")));
    }
    s
}

/// Recursively scan a JSON value for `@name` strings — these are the
/// node/asset references the style spec uses for cross-node wiring.
fn collect_refs(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => {
            if let Some(rest) = s.strip_prefix('@') {
                out.push(rest.to_string());
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_refs(x, out)),
        serde_json::Value::Object(m) => m.values().for_each(|x| collect_refs(x, out)),
        _ => {}
    }
}

async fn run_tile(args: TileCmd) -> Result<(), Box<dyn std::error::Error>> {
    let prep = prepare(&args.common).await?;
    let format = args
        .format
        .unwrap_or_else(|| OutputFormat::from_path(&args.out));
    let raster = render_one(
        Arc::clone(&prep.graph),
        Arc::clone(&prep.cache),
        Arc::clone(&prep.loader),
        prep.source.as_ref().map(Arc::clone),
        prep.source_name.as_ref().map(Arc::clone),
        Arc::clone(&prep.dem_sources),
        prep.canvas,
        args.tile,
        prep.overzoom_levels,
        Arc::clone(&prep.params),
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
    let format = args
        .format
        .unwrap_or_else(|| OutputFormat::from_path(&args.out));
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
            let source = prep.source.as_ref().map(Arc::clone);
            let source_name = prep.source_name.as_ref().map(Arc::clone);
            let dem_sources = Arc::clone(&prep.dem_sources);
            let canvas = prep.canvas;
            let overzoom_levels = prep.overzoom_levels;
            let params = Arc::clone(&prep.params);
            tasks.push(tokio::spawn(async move {
                let raster = render_one(
                    graph,
                    cache,
                    loader,
                    source,
                    source_name,
                    dem_sources,
                    canvas,
                    tile,
                    overzoom_levels,
                    params,
                )
                .await?;
                Ok::<(CoreTileId, Arc<RasterBuf>), Box<dyn std::error::Error + Send + Sync>>((
                    tile, raster,
                ))
            }));
        }
    }

    let mut mosaic = Pixmap::new(nx * prep.canvas.tile_size, ny * prep.canvas.tile_size)
        .ok_or("mosaic alloc")?;
    for handle in try_join_all(tasks).await? {
        let (tile, raster) = handle.map_err(|e| e.to_string())?;
        let dx = ((tile.x - x_range.start) * prep.canvas.tile_size) as i32;
        let dy = ((tile.y - y_range.start) * prep.canvas.tile_size) as i32;
        blit_padded_into(
            &mut mosaic,
            &raster,
            dx,
            dy,
            prep.canvas.tile_size,
            prep.canvas.pad,
        )?;
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
                    prep.source.as_ref().map(Arc::clone),
                    prep.source_name.as_ref().map(Arc::clone),
                    Arc::clone(&prep.dem_sources),
                    prep.canvas,
                    tile,
                    prep.overzoom_levels,
                    Arc::clone(&prep.params),
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
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                    e.to_string().into()
                })?;
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
        tracing::info!("z={z}: done in {:.1}s", t0.elapsed().as_secs_f64());
    }
    tracing::info!("wrote {total} tiles → {}", args.out.display());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn render_one(
    graph: Arc<Graph>,
    cache: Arc<Cache>,
    loader: Arc<BrushBankLoader>,
    source: Option<Arc<TileSource>>,
    source_name: Option<Arc<str>>,
    dem_sources: Arc<DemSourceRegistry>,
    canvas: CanvasInfo,
    tile: CoreTileId,
    overzoom_levels: u8,
    params: Arc<ParamValues>,
) -> Result<Arc<RasterBuf>, Box<dyn std::error::Error + Send + Sync>> {
    let fetched = match source {
        Some(s) => s.fetch_with_fallback(tile, overzoom_levels).await?,
        None => None,
    };
    let tile_id = TileId {
        z: tile.z,
        x: tile.x,
        y: tile.y,
    };
    // Pre-fetch DEM mosaics for the tile before entering spawn_blocking
    // so the blocking path doesn't have to juggle async fetches.
    let mut dem_bindings: Vec<(String, ezu::graph::ScalarField)> = Vec::new();
    if !dem_sources.is_empty() {
        let base_loader = BrushBankLoader::empty();
        let mut tmp = TileLoader::new(&base_loader, tile_id);
        bind_dem_sources(&mut tmp, &dem_sources, tile_id, canvas).await?;
        for name in dem_sources.names() {
            if let Ok(ezu::graph::Asset::ScalarField(field)) =
                ezu::graph::AssetLoader::load(&tmp, name)
            {
                dem_bindings.push((name.to_string(), (*field).clone()));
            }
        }
    }
    let raster = tokio::task::spawn_blocking(
        move || -> Result<Arc<RasterBuf>, Box<dyn std::error::Error + Send + Sync>> {
            let mut tile_loader = TileLoader::new(loader.as_ref(), tile_id);
            if let (Some((bytes, src_tile)), Some(src_name)) = (fetched, source_name) {
                let mut decoded = mvt::decode(&bytes)?;
                if src_tile != tile {
                    decoded = mvt::clip_to_descendant(&decoded, src_tile, tile)?;
                }
                tile_loader.bind_mvt(&src_name, decoded);
            }
            for (name, field) in dem_bindings {
                tile_loader.bind_scalar_field(name, field);
            }
            let ev = Evaluator::new(&graph, &cache, &tile_loader);
            let out = ev.render_parallel(tile_id, canvas, &params, tile_seed(tile))?;
            match out {
                PortValue::Raster(r) => Ok(r),
                other => Err(format!("expected Raster output, got {:?}", other.kind()).into()),
            }
        },
    )
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

/// Return the first MVT/Pmtiles entry in the style's `sources` block as
/// a [`SourceSpec`] the CLI can open. DEM sources are handled
/// separately. Returns `None` if no compatible source is declared;
/// when several are present the document order wins (later entries are
/// ignored with a warning).
pub(crate) struct FeatureSourcePick {
    pub name: String,
    pub spec: SourceSpec,
    pub origin: &'static str,
}

pub(crate) fn feature_source_from_doc(doc: &Document) -> Option<FeatureSourcePick> {
    let mut chosen: Option<FeatureSourcePick> = None;
    for (name, decl) in &doc.sources {
        let (spec, origin) = match decl {
            SourceDecl::Mvt(s) => (SourceSpec::Mvt(s.url.clone()), "style sources (mvt)"),
            SourceDecl::Pmtiles(s) => (
                SourceSpec::PmTiles(s.url.clone()),
                "style sources (pmtiles)",
            ),
            // Document-scoped and tile-scoped raster — not feature
            // sources, skip.
            SourceDecl::Brush(_) | SourceDecl::Image(_) | SourceDecl::Dem(_) => continue,
        };
        if chosen.is_some() {
            tracing::warn!("multiple feature sources in style; ignoring `{name}`");
            continue;
        }
        chosen = Some(FeatureSourcePick {
            name: name.clone(),
            spec,
            origin,
        });
    }
    chosen
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
        return Err(format!(
            "expected `min_lng,min_lat,max_lng,max_lat`, got `{s}`"
        ));
    }
    let v: Vec<f64> = parts
        .iter()
        .map(|p| {
            p.trim()
                .parse::<f64>()
                .map_err(|e| format!("bad number `{p}`: {e}"))
        })
        .collect::<Result<_, _>>()?;
    let (min_lng, min_lat, max_lng, max_lat) = (v[0], v[1], v[2], v[3]);
    if min_lng >= max_lng || min_lat >= max_lat {
        return Err(format!(
            "bbox min must be strictly less than max: {min_lng},{min_lat},{max_lng},{max_lat}"
        ));
    }
    Ok(BBox {
        min_lng,
        min_lat,
        max_lng,
        max_lat,
    })
}

fn tile_seed(tile: CoreTileId) -> u64 {
    let mut s = 0u64;
    s = s
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(tile.z as u64);
    s = s
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(tile.x as u64);
    s = s
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(tile.y as u64);
    s
}
