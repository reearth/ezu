//! Command-line renderer for the Ezu Style Spec.
//!
//! ```text
//! ezu tile --style STYLE.json --pmtiles URL_OR_PATH --tile Z/X/Y [--out FILE]
//! ezu bbox --style STYLE.json --mvt-url 'https://.../{z}/{x}/{y}.pbf' \
//!          --bbox MIN_LNG,MIN_LAT,MAX_LNG,MAX_LAT --zoom Z [--out FILE]
//! ezu frame --style STYLE.json --center LNG,LAT --scale 1000 --dpi 200 \
//!           --size-mm 245x174 [--out FILE]
//! ezu frame --style STYLE.json --pages PAGES.json
//! ```

mod frame_features;
mod frame_pages;
mod graph_view;
mod render;
mod serve;
pub(crate) mod source;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Args, Parser, Subcommand, ValueEnum};
use ezu::core::frame::{Frame, FrameRequest};
use ezu::core::TileId as CoreTileId;
use ezu::graph::{build_graph, Cache, CanvasInfo, Graph, ParamValues};
use ezu::paint::host::{pixmap_to_webp, BrushBankLoader};
use ezu::paint::nodes::default_registry;
use ezu::style::{Document, SourceDecl};
use futures::stream::{StreamExt, TryStreamExt};
use tiny_skia::{ColorU8, Pixmap};
use tracing_subscriber::EnvFilter;

use crate::render::TileRenderer;
use crate::source::{FeatureSources, SourceSpec, TileByteCache};

#[derive(Parser, Debug)]
#[command(name = "ezu", version, about = "Render Ezu Style documents to PNG")]
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
    /// Render a map at a print scale and paper size, cropped to that frame.
    Frame(FrameCmd),
    /// Bulk-render an XYZ tile pyramid into `<out>/<z>/<x>/<y>.png`.
    Tiles(TilesCmd),
    /// Validate an Ezu Style document without rendering — exits non-zero
    /// on parse / graph / asset errors. Suitable for CI + pre-commit hooks.
    Check(CheckCmd),
    /// Emit a Mermaid `graph LR` diagram of the style's node
    /// dependencies — or, with `--tile`, an HTML page of the same graph
    /// with each node's own intermediate tile drawn on it.
    Graph(GraphCmd),
    /// Emit the style's declared legend as JSON, for a host to lay out
    /// beside the map.
    Legend(LegendCmd),
    /// Translate a map-engine style (MapLibre GL) into an ezu recipe.
    Translate(TranslateCmd),
    /// Start the live editor + tile server at `http://127.0.0.1:8080`.
    Serve(serve::ServeCmd),
    /// Print the Ezu Style JSON Schema, gathered from the registered ops.
    Schema(SchemaCmd),
}

#[derive(Args, Debug)]
struct SchemaCmd {
    /// Write to this path instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,
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
    /// PMTiles archive — local path or http(s):// URL. Stands in for the
    /// first `mvt`/`pmtiles` source the style declares; any others keep
    /// their own URLs.
    #[arg(long, conflicts_with = "mvt")]
    pmtiles: Option<String>,
    /// Templated MVT tile source containing `{z}`, `{x}`, `{y}`
    /// placeholders. Accepts an http(s):// URL or a local path
    /// template (e.g. `/tiles/{z}/{x}/{y}.pbf`). Stands in for the first
    /// `mvt`/`pmtiles` source the style declares; any others keep their
    /// own URLs.
    #[arg(long, conflicts_with = "pmtiles")]
    mvt: Option<String>,
    /// When a requested tile is missing, fall back to a parent tile
    /// up to this many zoom levels up and re-project its geometry
    /// onto the requested tile (MVT "overzoom"). `0` disables. A source
    /// past its declared `max-zoom` overzooms from that zoom regardless.
    #[arg(long, default_value_t = 4)]
    overzoom_levels: u8,
    /// Override a document parameter, as `name=value` (repeatable).
    /// Values are validated against the style's `params` declarations:
    /// numbers respect `min`/`max`, colors are `#rrggbb[aa]`, bools
    /// are `true`/`false`.
    #[arg(long = "param", value_name = "NAME=VALUE")]
    params: Vec<String>,
    /// Refuse to render a style that raises build warnings — an
    /// unnamed `source`, a `$param` baked in at build time. For CI,
    /// where a warning is a style that will surprise someone later.
    #[arg(long)]
    strict: bool,
}

/// Output raster format. Pure-Rust pipelines on both sides — WebP is
/// lossless via the `image-webp` codec, typically 20–40 % smaller than
/// PNG for painterly content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
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

    /// `opts`, asking the renderer to encode in this format.
    fn with(self, opts: ezu::renderer::RenderOptions) -> ezu::renderer::RenderOptions {
        ezu::renderer::RenderOptions {
            format: match self {
                OutputFormat::Png => ezu::renderer::OutputFormat::Png,
                OutputFormat::Webp => ezu::renderer::OutputFormat::Webp,
            },
            ..opts
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
    /// Write the report to stdout as JSON instead of log lines: name,
    /// version, counts, padding, attribution, and the style's params
    /// schema. Logs move to stderr so the stream stays parseable.
    #[arg(long)]
    json: bool,
    /// Exit non-zero when the style raises build warnings, not just on
    /// errors. For CI, where a warning is a style that will surprise
    /// someone later.
    #[arg(long)]
    strict: bool,
}

#[derive(Args, Debug)]
struct GraphCmd {
    /// Ezu Style JSON document — local path or http(s):// URL.
    style: String,
    /// Output file. Writes to stdout when omitted.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Render this tile as `Z/X/Y` and draw each node's own
    /// intermediate result onto the diagram. Switches the output from
    /// Mermaid to a self-contained HTML viewer; `--out` is required.
    #[arg(long, value_parser = parse_zxy)]
    tile: Option<CoreTileId>,
    /// Base directory for resolving asset `src` paths. Defaults to the
    /// style file's parent directory (or the current directory when the
    /// style is a URL).
    #[arg(long)]
    assets_dir: Option<PathBuf>,
    /// PMTiles archive — local path or http(s):// URL.
    #[arg(long, conflicts_with = "mvt")]
    pmtiles: Option<String>,
    /// Templated MVT tile source containing `{z}`, `{x}`, `{y}`.
    #[arg(long, conflicts_with = "pmtiles")]
    mvt: Option<String>,
    /// Overzoom fallback depth for a missing tile, as in `ezu tile`.
    #[arg(long, default_value_t = 4)]
    overzoom_levels: u8,
    /// Override a document parameter, as `name=value` (repeatable).
    #[arg(long = "param", value_name = "NAME=VALUE")]
    params: Vec<String>,
    /// Long edge of each embedded node image, in pixels. `0` embeds
    /// them at the style's own tile size — sharper, and a much larger
    /// file on a graph of any size.
    #[arg(long, default_value_t = 256)]
    image_size: u32,
    /// Write the node images as PNG files beside the HTML (in
    /// `<out>.assets/`) instead of embedding them. Keeps the page
    /// small; pair with `--image-size 0` for full-size pictures.
    #[arg(long)]
    sidecar: bool,
}

#[derive(Args, Debug)]
struct LegendCmd {
    /// Ezu Style JSON document — local path or http(s):// URL.
    style: String,
    /// Keep only the entries that apply at this zoom. Also the zoom the
    /// swatches are drawn at, since a symbol may change with scale.
    #[arg(long)]
    zoom: Option<u8>,
    /// Output file. Writes to stdout when omitted.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Pretty-print the emitted JSON.
    #[arg(long)]
    pretty: bool,
    /// Draw each entry's symbol into this directory as a PNG, and add
    /// its path to that entry in the emitted JSON.
    #[arg(long)]
    swatch_dir: Option<PathBuf>,
    /// Swatch size as `WIDTHxHEIGHT` (or one number for a square).
    #[arg(long, default_value = "48x32", value_parser = parse_wxh)]
    swatch_size: (u32, u32),
    /// Base directory for resolving relative asset `src` paths, for
    /// swatches whose symbol needs a brush, font or sprite.
    #[arg(long)]
    assets_dir: Option<PathBuf>,
}

/// Parse `WIDTHxHEIGHT`, or a single number as a square.
fn parse_wxh(s: &str) -> Result<(u32, u32), String> {
    let parse = |v: &str| {
        v.trim()
            .parse::<u32>()
            .map_err(|_| format!("`{s}`: expected WIDTHxHEIGHT in whole pixels"))
            .and_then(|n| (n > 0).then_some(n).ok_or_else(|| format!("`{s}`: zero")))
    };
    match s.split_once(['x', 'X']) {
        Some((w, h)) => Ok((parse(w)?, parse(h)?)),
        None => {
            let n = parse(s)?;
            Ok((n, n))
        }
    }
}

#[derive(Args, Debug)]
struct TranslateCmd {
    /// Source map-engine style (MapLibre GL JSON) — local path or
    /// http(s):// URL.
    style: String,
    /// Output file for the ezu recipe. Writes to stdout when omitted.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Emitted `tile-size` (MapLibre uses 512).
    #[arg(long, default_value_t = 512)]
    tile_size: u32,
    /// Emitted `pad` — the margin for geometry painted wider than its own
    /// extent (a thick stroke) before the crop. Filter reach (blur, warp,
    /// mosaic, label extent) is sized by the renderer from the graph, so
    /// this is only about paint width, which an expression can decide per
    /// feature. The default covers a typical basemap's line widths.
    #[arg(long, default_value_t = 16)]
    pad: u32,
    /// Keep `visibility: none` layers in the recipe, gated off behind a
    /// `switch` (instead of dropping them).
    #[arg(long)]
    keep_hidden: bool,
    /// Migrate the style's legacy forms to expressions first (legacy
    /// filters, `{stops}` function objects, `{token}` label strings,
    /// non-standard `hsl()` colours), as MapLibre's own `migrate` does.
    /// Without it a legacy form is passed through untouched, with a
    /// warning naming it. Requires a `version: 8` style.
    #[arg(long)]
    migrate: bool,
    /// Map a MapLibre fontstack entry to a font source, as `NAME=SOURCE`
    /// (repeatable). SOURCE is an installed-font reference
    /// (`system:Helvetica`, optionally `?weight=700&style=italic`) or a
    /// font-file URL (`http(s)://…`, `file:…`, `data:…`) — e.g.
    /// `--font "Noto Sans Regular=system:Noto Sans"` or
    /// `--font "Noto Sans Regular=https://example.com/NotoSans-Regular.ttf"`.
    /// Optional: unmapped `text-font` stacks fall back to the style's
    /// `glyphs` endpoint (SDF glyph ranges); a mapping wins where
    /// present and renders from the real font.
    #[arg(long = "font", value_name = "NAME=SOURCE")]
    fonts: Vec<String>,
    /// Pretty-print the emitted JSON.
    #[arg(long)]
    pretty: bool,
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
struct FrameCmd {
    #[command(flatten)]
    common: CommonArgs,
    /// Render every page a JSON file lists, in place of the one page the
    /// flags below describe. Each page takes the fields `center`, `scale`,
    /// `dpi`, `size-mm` or `size-px`, `zoom`, `out`, `format`,
    /// `features-coords` and `features` (`{NODE: {"out": PATH, "key":
    /// [PROP, ...]}}`), any of them but `out` from a `defaults` object. The
    /// style is read once, and pages over the same tiles fetch them once.
    #[arg(long, value_name = "PATH", conflicts_with_all = [
        "center", "scale", "dpi", "size_mm", "size_px", "zoom", "out", "format",
        "features", "features_key", "features_coords",
    ])]
    pages: Option<PathBuf>,
    /// Centre of the frame as `lng,lat` (WGS84). The scale is matched at
    /// this latitude.
    #[arg(long, value_parser = parse_lnglat, required_unless_present = "pages")]
    center: Option<(f64, f64)>,
    /// Scale denominator: `1000` for 1:1000.
    #[arg(long, required_unless_present = "pages")]
    scale: Option<f64>,
    /// Output resolution in dots per inch. Always required: with the scale
    /// it fixes how many ground metres one pixel covers. It does not
    /// enlarge the style's own pixel sizes (line widths, text).
    #[arg(long, required_unless_present = "pages")]
    dpi: Option<f64>,
    /// Paper size as `WIDTHxHEIGHT` in millimetres, converted to pixels at
    /// `--dpi`.
    #[arg(long, value_parser = parse_size_mm, required_unless_present_any = ["size_px", "pages"], conflicts_with = "size_px")]
    size_mm: Option<(f64, f64)>,
    /// Output size as `WIDTHxHEIGHT` in pixels, in place of `--size-mm`.
    #[arg(long, value_parser = parse_wxh)]
    size_px: Option<(u32, u32)>,
    /// Render at this zoom, deriving the tile size from it. By default the
    /// zoom is the one whose tile size comes nearest the style's
    /// `tile-size`.
    #[arg(long)]
    zoom: Option<u8>,
    /// Output path. Format is sniffed from the extension (`.png` /
    /// `.webp`); use `--format` to override.
    #[arg(long, default_value = "out.png")]
    out: PathBuf,
    /// Output format. Defaults to whatever `--out`'s extension implies.
    #[arg(long, value_enum)]
    format: Option<OutputFormat>,
    /// Number of tiles rendered in parallel. Defaults to the number of
    /// logical CPU cores.
    #[arg(long)]
    concurrency: Option<usize>,
    /// Write the features node NODE, gathered over every tile the frame
    /// renders, as a GeoJSON FeatureCollection at PATH. Features whose
    /// bounding box lies wholly outside the frame are left out; the rest
    /// are written whole, not clipped. Without `--features-key`, a feature
    /// several tiles see is written once per tile, and a shape cut at tile
    /// edges as its pieces. Repeatable.
    #[arg(long = "features", value_name = "NODE=PATH", value_parser = frame_features::parse_node_path)]
    features: Vec<(String, PathBuf)>,
    /// Treat features of NODE whose listed properties are all equal as one
    /// feature, keeping the first in tile order (rows top to bottom, then
    /// left to right), and sort the file by those properties. For
    /// `label-boxes`: `--features-key boxes=label-id,part`. Repeatable,
    /// once per node.
    #[arg(long, value_name = "NODE=PROP[,PROP...]", value_parser = frame_features::parse_node_key)]
    features_key: Vec<(String, Vec<String>)>,
    /// Coordinates of the `--features` files: `lonlat` (WGS84, 7 decimals)
    /// or `px`, pixels of the output image from its top-left corner, y
    /// down (2 decimals).
    #[arg(long, value_enum, default_value_t = frame_features::FeatureCoords::Lonlat)]
    features_coords: frame_features::FeatureCoords,
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
    // Resolved at run time rather than through `default_value_t`, so
    // `--help` does not print the core count of whichever machine ran it.
    #[arg(long)]
    concurrency: Option<usize>,
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

/// Heap profiler, wired in only under `--features heap-profile`. Writes
/// `dhat-heap.json` (viewable at <https://nnethercote.github.io/dh_view/>)
/// on exit, attributing peak heap to allocation sites — the tool to reach
/// for when a render's memory is not explained by its pixel buffers.
#[cfg(feature = "heap-profile")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[tokio::main]
async fn main() {
    #[cfg(feature = "heap-profile")]
    let _dhat = dhat::Profiler::new_heap();
    // Returning the error from `main` would print it with `Debug`, which
    // for a `thiserror` type means the struct dump rather than the
    // `#[error(...)]` sentence written for exactly this moment. Print
    // `Display` instead, then the chain of `source()`s, so a nested
    // parse failure reads as the story it is.
    if let Err(e) = run().await {
        let top = e.to_string();
        eprintln!("error: {top}");
        let mut shown = top;
        let mut src = e.source();
        while let Some(cause) = src {
            let text = cause.to_string();
            // Several of these errors interpolate their source into their
            // own message, so printing the chain verbatim repeats it.
            if !shown.contains(&text) {
                eprintln!("  caused by: {text}");
            }
            shown = text;
            src = cause.source();
        }
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let filter = if cli.verbose {
        // Bump just the per-node evaluator target — info elsewhere keeps
        // the noise focused on the graph trace the user asked for.
        EnvFilter::new("info,ezu_graph::eval=debug")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
    };
    // Logs go to stderr, always: `check --json`, `legend` and
    // `translate` write their payload to stdout, and a pipe into `jq`
    // has to see JSON and nothing else.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    match cli.cmd {
        Cmd::Tile(args) => run_tile(args).await,
        Cmd::Bbox(args) => run_bbox(args).await,
        Cmd::Frame(args) => run_frame(args).await,
        Cmd::Tiles(args) => run_tiles(args).await,
        Cmd::Check(args) => run_check(args).await,
        Cmd::Graph(args) => run_graph(args).await,
        Cmd::Legend(args) => run_legend(args).await,
        Cmd::Translate(args) => run_translate(args).await,
        Cmd::Serve(args) => serve::run(args).await,
        Cmd::Schema(args) => run_schema(args),
    }
}

/// Read the style `common` names and open it for rendering, keeping the
/// source tiles fetched for one render for the next ones.
async fn open(common: &CommonArgs) -> Result<Opened, Box<dyn std::error::Error>> {
    let style_text = fetch_text(&common.style).await?;
    let doc = Document::from_json(&style_text)?;
    let byte_cache = Arc::new(TileByteCache::new(tile_cache_budget_bytes()));
    open_style(common, &style_text, &doc, &byte_cache).await
}

/// A style opened for rendering: the document and its graph, which is what
/// the CLI reports from and checks requests against, and a renderer over
/// the style's sources.
struct Opened {
    graph: Arc<Graph>,
    tiles: TileRenderer,
}

/// Open the style that `doc` and `style_text` hold, without reading it
/// again: build the graph, report on it, open every source the style
/// declares, keeping fetched tile bytes in `byte_cache`, and let the
/// renderer read the style's assets.
async fn open_style(
    common: &CommonArgs,
    style_text: &str,
    doc: &Document,
    byte_cache: &Arc<TileByteCache>,
) -> Result<Opened, Box<dyn std::error::Error>> {
    tracing::info!(
        "style: {} v{} ({} nodes, tile={})",
        doc.name,
        doc.version,
        doc.nodes.len(),
        doc.tile_size,
    );
    let assets_dir = assets_dir_for(&common.style, common.assets_dir.as_deref());
    let graph = Arc::new(build_graph(doc, &default_registry())?);
    report_pad(&graph, doc);
    // Checked here so a bad value fails before anything is fetched; the
    // renderer checks each render's the same way.
    parse_cli_params(&common.params, doc)?;

    // CLI flags override the URL but keep the doc's source NAME, since
    // the style's `features` nodes reference sources by name.
    let cli_override = match (&common.pmtiles, &common.mvt) {
        (Some(p), None) => Some((SourceSpec::PmTiles(p.clone()), "--pmtiles flag")),
        (None, Some(u)) => Some((SourceSpec::Mvt(u.clone()), "--mvt flag")),
        (None, None) => None,
        _ => return Err("--pmtiles and --mvt are mutually exclusive".into()),
    };
    let vector = FeatureSources::open(doc, cli_override, &assets_dir)
        .await?
        .with_byte_cache(byte_cache, false);
    let tiles = TileRenderer::open(render::Open {
        style: style_text,
        doc,
        requested: graph.asset_inputs(),
        pad: canvas_pad(&graph, doc),
        assets_dir: &assets_dir,
        vector: Arc::new(vector),
        overzoom_levels: common.overzoom_levels,
        params: common
            .params
            .iter()
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        byte_cache: Some((byte_cache, false)),
        decoded_budget: DECODED_CACHE_BUDGET,
        cache_budget: cache_budget_bytes(),
    })
    .await?;
    // The renderer has logged the graph's warnings as it built.
    strict_verdict(common.strict, graph.warnings())?;
    tracing::info!("assets resolved against {}", assets_dir.display());
    Ok(Opened { graph, tiles })
}

/// How many bytes of fetched source tiles stay decoded for the renders
/// around the one that decoded them. Charged by the size of the encoded
/// tile, which is a fraction of the decoded one, so this holds a few
/// hundred tiles — rows of a pyramid, or the tiles a run of pages shares.
const DECODED_CACHE_BUDGET: usize = 32 * 1024 * 1024;

/// The directory `file:` paths in the style resolve against: `--assets-dir`,
/// or the style file's own directory, or the current one for a style read
/// from a URL.
fn assets_dir_for(style: &str, flag: Option<&Path>) -> PathBuf {
    match flag {
        Some(dir) => dir.to_path_buf(),
        None if is_url(style) => PathBuf::from("."),
        None => Path::new(style)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".")),
    }
}

/// Copy `rgba`, a `tile_size`-square tile of straight RGBA, into `dst`
/// with its top-left at `(dx, dy)`, keeping only the part that falls
/// inside `dst`.
///
/// The offset may be negative or run past the far edge: a print frame takes
/// just a corner of the tiles along its border. Each pixel is
/// premultiplied back into the image as it is copied, which gives back
/// exactly the renderer's own pixels: they were demultiplied from those,
/// and premultiplying a demultiplied pixel is exact.
fn blit_rgba_into(dst: &mut Pixmap, rgba: &[u8], dx: i64, dy: i64, tile_size: u32) {
    let t = tile_size as i64;
    let (dw, dh) = (dst.width() as i64, dst.height() as i64);
    let (x0, x1) = (dx.max(0), (dx + t).min(dw));
    let (y0, y1) = (dy.max(0), (dy + t).min(dh));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let pixels = dst.pixels_mut();
    for y in y0..y1 {
        let src_row = ((y - dy) * t) as usize;
        let dst_row = (y * dw) as usize;
        for x in x0..x1 {
            let s = (src_row + (x - dx) as usize) * 4;
            pixels[dst_row + x as usize] =
                ColorU8::from_rgba(rgba[s], rgba[s + 1], rgba[s + 2], rgba[s + 3]).premultiply();
        }
    }
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

/// Dump the document JSON Schema assembled from every registered op —
/// the same document served at `/schemas/ezu-style.json` by `ezu serve`.
/// Feed it to editor tooling, an `ajv` CI check, or a docs generator.
fn run_schema(args: SchemaCmd) -> Result<(), Box<dyn std::error::Error>> {
    let schema = default_registry().document_schema();
    let mut text = serde_json::to_string_pretty(&schema)?;
    text.push('\n');
    match args.out {
        Some(path) => std::fs::write(&path, text)?,
        None => print!("{text}"),
    }
    Ok(())
}

/// The canvas margin to render with: whichever is larger of what the
/// style asks for and what its filters actually read.
///
/// A style's `pad` is a floor, not the answer. Rendering happens on one
/// canvas of `tile-size + 2 * pad` and nothing grows it mid-render, so a
/// margin narrower than the furthest a node reaches leaves the tile's own
/// edge pixels computed from clamped data — a seam between tiles rather
/// than an error. The graph knows the distance, so use it, and let a
/// style that wants a deliberately wider margin keep it.
pub(crate) fn canvas_pad(graph: &Graph, doc: &Document) -> u32 {
    match graph.required_pad() {
        Ok(needed) => doc.pad.max(needed),
        // Only a graph over `MAX_PAD` gets here; `report_pad` has already
        // said so, and rendering with what the style asked for is a more
        // useful answer than refusing.
        Err(_) => doc.pad,
    }
}

/// Say what the canvas margin will be and where it came from.
fn report_pad(graph: &Graph, doc: &Document) {
    let needed = match graph.required_pad() {
        Ok(needed) => needed,
        Err(e) => {
            tracing::warn!("pad: {e}");
            return;
        }
    };
    if needed > doc.pad {
        tracing::info!(
            "pad: {} declared, {needed} needed — rendering with {needed}",
            doc.pad,
        );
    } else {
        tracing::info!("pad: {} declared, {needed} needed", doc.pad);
    }
}

/// Machine-readable `ezu check` report — `--json`.
///
/// `params` is the same JSON Schema the tile server serves at
/// `/style/params` and the wasm renderer returns from `paramsSchema`.
/// A host that generates its own params panel can diff this in CI and
/// be told when a style grows a knob the UI has no control for, instead
/// of finding out from a widget that quietly went missing.
#[derive(serde::Serialize)]
struct CheckReport<'a> {
    name: &'a str,
    version: &'a str,
    nodes: usize,
    sources: usize,
    pad: PadReport,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attribution: Vec<&'a str>,
    params: serde_json::Value,
    /// True when asset `src` entries were resolved too — the inverse of
    /// `--no-fetch`, stated so a consumer knows how much this pass
    /// actually covered.
    assets_resolved: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

#[derive(serde::Serialize)]
struct PadReport {
    declared: u32,
    /// What the graph needs. Absent when it could not be computed — the
    /// reason is then in `warnings`.
    #[serde(skip_serializing_if = "Option::is_none")]
    needed: Option<u32>,
}

async fn run_check(args: CheckCmd) -> Result<(), Box<dyn std::error::Error>> {
    let text = fetch_text(&args.style).await?;
    let doc = Document::from_json(&text)?;
    let registry = default_registry();
    let graph = build_graph(&doc, &registry)?;
    let mut warnings = Vec::new();
    let needed = match graph.required_pad() {
        Ok(needed) => Some(needed),
        Err(e) => {
            warnings.push(format!("pad: {e}"));
            None
        }
    };
    warnings.extend(graph.warnings().iter().cloned());
    let attributions = doc.attributions();
    if !args.json {
        match needed {
            Some(needed) if needed > doc.pad => tracing::info!(
                "pad: {} declared, {needed} needed — rendering with {needed}",
                doc.pad,
            ),
            Some(needed) => tracing::info!("pad: {} declared, {needed} needed", doc.pad),
            None => {}
        }
        for w in &warnings {
            tracing::warn!("{w}");
        }
        if !attributions.is_empty() {
            tracing::info!("attribution: {}", attributions.join(" | "));
        }
    }

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

    if args.json {
        let report = CheckReport {
            name: &doc.name,
            version: &doc.version,
            nodes: graph.len(),
            sources: doc.sources.len(),
            pad: PadReport {
                declared: doc.pad,
                needed,
            },
            attribution: attributions,
            params: doc.params_schema(),
            assets_resolved: !args.no_fetch,
            warnings: warnings.clone(),
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
        return strict_verdict(args.strict, &warnings);
    }

    strict_verdict(args.strict, &warnings)?;

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

/// `--strict` turns the run's warnings into a non-zero exit. The report
/// is already out by the time this is called — a CI job gets both the
/// full picture and the verdict.
fn strict_verdict(strict: bool, warnings: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if strict && !warnings.is_empty() {
        return Err(format!("--strict: {} warning(s); see above", warnings.len()).into());
    }
    Ok(())
}

/// Number of document-scoped sources (brush / image / font / glyphs) —
/// the ones `prefetch_doc_assets` stages on style load.
fn count_doc_scoped_sources(doc: &Document) -> usize {
    doc.sources
        .values()
        .filter(|d| {
            matches!(
                d,
                SourceDecl::Brush(_)
                    | SourceDecl::Image(_)
                    | SourceDecl::Font(_)
                    | SourceDecl::Glyphs(_)
            )
        })
        .count()
}

async fn run_graph(args: GraphCmd) -> Result<(), Box<dyn std::error::Error>> {
    if args.tile.is_some() {
        return run_graph_view(args).await;
    }
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

/// `ezu graph --tile Z/X/Y`: render the tile once, watching every node,
/// and write the diagram with each node's own picture on it.
///
/// The diagram here is of the *expanded* graph — the one that actually
/// evaluated — so a function call's body shows up as `call/body` nodes.
/// The viewer folds those onto the call by default, which reads like the
/// Mermaid diagram does, and opens them on demand.
async fn run_graph_view(args: GraphCmd) -> Result<(), Box<dyn std::error::Error>> {
    let tile = args.tile.expect("checked by the caller");
    let Some(out) = args.out.clone() else {
        return Err("`--tile` writes an HTML page; give it a path with `--out`".into());
    };

    let common = CommonArgs {
        style: args.style.clone(),
        assets_dir: args.assets_dir.clone(),
        pmtiles: args.pmtiles.clone(),
        mvt: args.mvt.clone(),
        overzoom_levels: args.overzoom_levels,
        params: args.params.clone(),
        // A diagram of what the graph does; warnings are worth seeing
        // here, not worth refusing to draw over.
        strict: false,
    };
    let doc = Document::from_json(&fetch_text(&common.style).await?)?;
    let opened = open(&common).await?;
    let tiles = &opened.tiles;

    let canvas = CanvasInfo::square(tiles.tile_size(), tiles.pad());
    let collector = Arc::new(graph_view::Collector::new(canvas, args.image_size));
    let watching = Arc::clone(&collector);
    tiles
        .render(tile, move |r, set, mut opts| {
            // The pictures come from the nodes; the tile's own pixels
            // are not written, so skip encoding them.
            opts.format = ezu::renderer::OutputFormat::Rgba;
            r.render_tile_observed(tile.z, tile.x, tile.y, set, opts, &*watching)
        })
        .await
        .map_err(|e| e.to_string())?;

    // One reference is held by the render that just finished; take the
    // collector back now that it has.
    let collected = Arc::try_unwrap(collector)
        .map_err(|_| "the render still holds the node collector")?
        .finish();

    let sidecar = args.sidecar.then(|| {
        let mut dir = out.clone();
        dir.set_extension("assets");
        dir
    });
    let mut params = serde_json::Map::new();
    for raw in &args.params {
        if let Some((name, value)) = raw.split_once('=') {
            params.insert(
                name.to_string(),
                serde_json::Value::String(value.to_string()),
            );
        }
    }
    let payload = collected.into_payload(
        &doc,
        &opened.graph,
        &format!("{}/{}/{}", tile.z, tile.x, tile.y),
        &params,
        sidecar.as_deref(),
    )?;
    let html = graph_view::render_html(&payload);
    std::fs::write(&out, &html)?;
    tracing::info!(
        "wrote {} ({:.1} KiB, {} nodes)",
        out.display(),
        html.len() as f64 / 1024.0,
        opened.graph.len(),
    );
    Ok(())
}

async fn run_legend(args: LegendCmd) -> Result<(), Box<dyn std::error::Error>> {
    let text = fetch_text(&args.style).await?;
    let doc = Document::from_json(&text)?;
    // Build the graph too: it is what verifies that every entry names a
    // node that exists and draws something, so a legend emitted here has
    // already been checked against the map it describes.
    build_graph(&doc, &default_registry())?;

    let Some(legend) = &doc.legend else {
        return Err(format!("{} declares no `legend` block", args.style).into());
    };
    // Serialized straight from the declaration, never through
    // `serde_json::Value`, so the emitted keys keep the order the type
    // declares them in rather than coming out alphabetized.
    let filtered = args.zoom.map(|z| ezu::style::LegendDecl {
        title: legend.title.clone(),
        note: legend.note.clone(),
        entries: legend.entries_at(z).cloned().collect(),
    });
    let legend = filtered.as_ref().unwrap_or(legend);

    let swatches = match &args.swatch_dir {
        Some(dir) => draw_swatches(&doc, legend, dir, &args).await?,
        None => vec![None; legend.entries.len()],
    };
    let out = LegendOut {
        title: legend.title.as_deref(),
        note: legend.note.as_deref(),
        entries: legend
            .entries
            .iter()
            .zip(&swatches)
            .map(|(e, swatch)| EntryOut {
                label: &e.label,
                from: &e.from,
                properties: &e.properties,
                note: e.note.as_deref(),
                min_zoom: e.min_zoom,
                max_zoom: e.max_zoom,
                geometry: e.geometry,
                features: e.features.as_deref(),
                swatch: swatch.as_deref(),
            })
            .collect(),
    };
    let json = if args.pretty {
        serde_json::to_string_pretty(&out)?
    } else {
        serde_json::to_string(&out)?
    };
    match &args.out {
        Some(p) => {
            std::fs::write(p, &json)?;
            tracing::info!("wrote {} ({} bytes)", p.display(), json.len());
        }
        None => println!("{json}"),
    }
    Ok(())
}

/// The emitted legend: the declaration as written, plus where each
/// entry's swatch was drawn.
///
/// Spelled out rather than `#[serde(flatten)]`-ed onto `LegendEntry`,
/// because flattening routes serialization through a `serde_json::Map`,
/// which sorts its keys — and a legend read by a human wants its title
/// before its entries.
#[derive(serde::Serialize)]
#[serde(rename_all = "kebab-case")]
struct LegendOut<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'a str>,
    entries: Vec<EntryOut<'a>>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "kebab-case")]
struct EntryOut<'a> {
    label: &'a str,
    from: &'a ezu::style::NodeRef,
    /// As declared, swatch positions included: what longitude a
    /// `{ "swatch-x": … }` stands for depends on the zoom a swatch is
    /// drawn at, which is the host's choice when it draws its own.
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    properties: &'a serde_json::Map<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_zoom: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_zoom: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    geometry: Option<ezu::style::LegendGeometry>,
    /// Passed through as declared, so a host drawing its own swatches
    /// stands in the same features `--swatch-dir` does.
    #[serde(skip_serializing_if = "Option::is_none")]
    features: Option<&'a [ezu::style::LegendFeature]>,
    /// Path of the PNG this entry's symbol was drawn to, when
    /// `--swatch-dir` asked for one.
    #[serde(skip_serializing_if = "Option::is_none")]
    swatch: Option<&'a str>,
}

/// Draw every entry's symbol into `dir`, returning each one's path.
///
/// One cache serves the whole legend: entries that share upstream nodes
/// share the work, and the entry's own identity is in the cache key so
/// they cannot be confused for one another.
async fn draw_swatches(
    doc: &Document,
    legend: &ezu::style::LegendDecl,
    dir: &Path,
    args: &LegendCmd,
) -> Result<Vec<Option<String>>, Box<dyn std::error::Error>> {
    use ezu::paint::host::{crop_to_png, PngCompression};
    use ezu::paint::legend::{render_swatch, SwatchOptions};

    std::fs::create_dir_all(dir)?;
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
    let assets = build_asset_loader(doc, &base_dir).await?;
    let registry = default_registry();
    let cache = Cache::new();
    let params = ParamValues::new();
    let (width, height) = args.swatch_size;
    let opts = SwatchOptions {
        width,
        height,
        // A symbol may change with scale, so a swatch is only true for
        // one zoom. `--zoom` picks it; without one, mid-scale.
        zoom: args.zoom.unwrap_or(12),
        pad: 0,
        geometry: Default::default(),
    };

    let mut out = Vec::with_capacity(legend.entries.len());
    for (i, entry) in legend.entries.iter().enumerate() {
        let (raster, canvas) =
            render_swatch(doc, entry, &registry, &assets, &params, &cache, &opts)?;
        let png = crop_to_png(
            &raster,
            canvas.tile_w,
            canvas.tile_h,
            canvas.pad,
            PngCompression::Default,
        )?;
        let path = dir.join(format!("{i}.png"));
        std::fs::write(&path, &png)?;
        tracing::info!("wrote {} ({} bytes)", path.display(), png.len());
        out.push(Some(path.to_string_lossy().into_owned()));
    }
    Ok(out)
}

async fn run_translate(args: TranslateCmd) -> Result<(), Box<dyn std::error::Error>> {
    let text = fetch_text(&args.style).await?;
    let style: serde_json::Value = serde_json::from_str(&text)?;
    let mut fonts = std::collections::HashMap::new();
    for entry in &args.fonts {
        let Some((name, url)) = entry.split_once('=') else {
            return Err(format!("--font `{entry}`: expected NAME=URL").into());
        };
        fonts.insert(name.trim().to_string(), url.trim().to_string());
    }
    let opts = ezu::translate::maplibre::ConvertOptions {
        tile_size: args.tile_size,
        pad: args.pad,
        keep_hidden: args.keep_hidden,
        migrate: args.migrate,
        fonts,
    };
    let (recipe, report) = ezu::translate::maplibre::convert(&style, &opts)?;

    // Surface everything the converter skipped or approximated on stderr, so
    // it doesn't pollute the recipe written to stdout.
    for w in &report.warnings {
        tracing::warn!("{w}");
    }

    let json = if args.pretty {
        serde_json::to_string_pretty(&recipe)?
    } else {
        serde_json::to_string(&recipe)?
    };
    match &args.out {
        Some(p) => {
            std::fs::write(p, &json)?;
            tracing::info!(
                "wrote {} ({} bytes, {} warnings)",
                p.display(),
                json.len(),
                report.warnings.len()
            );
        }
        None => println!("{json}"),
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
            SourceDecl::Raster(_) => "raster",
            SourceDecl::GeoJson(_) => "geojson",
            SourceDecl::Sprite(_) => "sprite",
            SourceDecl::Font(_) => "font",
            SourceDecl::Glyphs(_) => "glyphs",
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
        let mut seen = HashSet::new();
        for r in spec.refs() {
            if !seen.insert(r.clone()) {
                continue;
            }
            if doc.nodes.contains_key(&r) || doc.sources.contains_key(&r) {
                s.push_str(&format!("  {r} --> {id}\n"));
            }
        }
    }

    // Each class fixes its own text colour as well as its fill. Mermaid's dark
    // theme otherwise draws light label text, which is unreadable on these
    // deliberately light fills — and a diagram rendered dark is the common case
    // when it is embedded in a dark page.
    s.push_str("\n  classDef asset fill:#fff4d6,color:#3a2e00,stroke:#a88500;\n");
    s.push_str("  classDef output fill:#ffe0e0,color:#4a1010,stroke:#cc3333,stroke-width:2px;\n");
    s.push_str("  classDef sink fill:#cc3333,color:#ffffff,stroke:#7a1f1f,stroke-width:2px;\n");
    s.push_str("  classDef source fill:#d9ecff,color:#0d2b45,stroke:#2a6fb0;\n");
    if !doc_scoped_ids.is_empty() {
        s.push_str(&format!("  class {} asset;\n", doc_scoped_ids.join(",")));
    }
    if !source_ids.is_empty() {
        s.push_str(&format!("  class {} source;\n", source_ids.join(",")));
    }
    s
}

async fn run_tile(args: TileCmd) -> Result<(), Box<dyn std::error::Error>> {
    let opened = open(&args.common).await?;
    let format = args
        .format
        .unwrap_or_else(|| OutputFormat::from_path(&args.out));
    let bytes = opened
        .tiles
        .render(args.tile, move |r, set, opts| {
            let t = args.tile;
            r.render_tile_with(t.z, t.x, t.y, set, format.with(opts))
        })
        .await
        .map_err(|e| e.to_string())?;
    std::fs::write(&args.out, &bytes)?;
    tracing::info!("wrote {} ({} bytes)", args.out.display(), bytes.len());
    Ok(())
}

/// Render `tile` as straight RGBA, for copying into a larger image.
async fn render_rgba(tiles: &TileRenderer, tile: CoreTileId) -> Result<Vec<u8>, render::BoxError> {
    tiles
        .render(tile, move |r, set, mut opts| {
            opts.format = ezu::renderer::OutputFormat::Rgba;
            r.render_tile_with(tile.z, tile.x, tile.y, set, opts)
        })
        .await
}

async fn run_bbox(args: BboxCmd) -> Result<(), Box<dyn std::error::Error>> {
    let opened = open(&args.common).await?;
    let tiles = &opened.tiles;
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

    let t = tiles.tile_size();
    let mut mosaic = Pixmap::new(nx * t, ny * t).ok_or("mosaic alloc")?;
    let xr = x_range.clone();
    let coords = y_range
        .clone()
        .flat_map(move |ty| xr.clone().map(move |tx| CoreTileId::new(args.zoom, tx, ty)));
    let mut rendered =
        futures::stream::iter(coords)
            .map(|tile| async move {
                Ok::<_, render::BoxError>((tile, render_rgba(tiles, tile).await?))
            })
            .buffer_unordered(default_concurrency());
    while let Some(done) = rendered.next().await {
        let (tile, rgba) = done.map_err(|e| e.to_string())?;
        let dx = (tile.x - x_range.start) as i64 * t as i64;
        let dy = (tile.y - y_range.start) as i64 * t as i64;
        blit_rgba_into(&mut mosaic, &rgba, dx, dy, t);
    }
    let bytes = match format {
        OutputFormat::Png => mosaic.encode_png().map_err(|e| e.to_string())?,
        OutputFormat::Webp => pixmap_to_webp(&mosaic).map_err(|e| e.to_string())?,
    };
    std::fs::write(&args.out, &bytes)?;
    tracing::info!("wrote {} ({} bytes)", args.out.display(), bytes.len());
    Ok(())
}

async fn run_frame(args: FrameCmd) -> Result<(), Box<dyn std::error::Error>> {
    let concurrency = args.concurrency.unwrap_or_else(default_concurrency);
    if concurrency == 0 {
        return Err("--concurrency must be ≥ 1".into());
    }
    let cache = Arc::new(TileByteCache::new(tile_cache_budget_bytes()));
    match &args.pages {
        Some(path) => {
            let pages = frame_pages::read(path)?;
            render_pages(&args.common, &pages, concurrency, true, &cache).await
        }
        None => {
            let page = page_from_flags(&args)?;
            render_pages(&args.common, &[page], concurrency, false, &cache).await
        }
    }
}

/// The one page the single-page flags describe.
fn page_from_flags(args: &FrameCmd) -> Result<frame_pages::Page, Box<dyn std::error::Error>> {
    let (Some(center), Some(scale), Some(dpi)) = (args.center, args.scale, args.dpi) else {
        return Err("--center, --scale and --dpi are required without --pages".into());
    };
    if !(dpi.is_finite() && dpi > 0.0) {
        return Err(format!("--dpi must be a positive number, got {dpi}").into());
    }
    let format = args
        .format
        .unwrap_or_else(|| OutputFormat::from_path(&args.out));
    let (width, height) = match (args.size_px, args.size_mm) {
        (Some(px), _) => px,
        (None, Some((w, h))) => (mm_to_px(w, dpi), mm_to_px(h, dpi)),
        (None, None) => return Err("one of --size-mm or --size-px is required".into()),
    };
    Ok(frame_pages::Page {
        center,
        scale,
        dpi,
        width,
        height,
        zoom: args.zoom,
        out: args.out.clone(),
        format,
        features: args.features.clone(),
        features_key: args.features_key.clone(),
        features_coords: args.features_coords,
    })
}

/// Render `pages` one after another from the style in `common`, each page's
/// tiles `concurrency` at a time.
///
/// The style is read, and its sources opened, once for every page. The
/// graph's intermediate cache is sized by the tile, so pages at the same
/// tile size share one and a page at another size starts a fresh one,
/// dropping the last: memory stays at one cache however many tile sizes the
/// job has, and a job ordered by scale keeps the most of it. Source tiles
/// fetched for one page are kept for the next, as fetched bytes in
/// `byte_cache` and as decoded data. Every page writes what a single-page
/// run of it would.
///
/// `many` marks a pages-file run: errors then name the page, and each page
/// and the whole job log a summary.
async fn render_pages(
    common: &CommonArgs,
    pages: &[frame_pages::Page],
    concurrency: usize,
    many: bool,
    byte_cache: &Arc<TileByteCache>,
) -> Result<(), Box<dyn std::error::Error>> {
    let on_page = |i: usize, e: Box<dyn std::error::Error>| -> Box<dyn std::error::Error> {
        if many {
            format!("page {}: {e}", i + 1).into()
        } else {
            e
        }
    };
    // The frame's tile size depends on the style's own, so the document is
    // read before anything is sized from it.
    let style_text = fetch_text(&common.style).await?;
    let doc = Document::from_json(&style_text)?;
    let style_tile_size = doc.tile_size;
    let frames = pages
        .iter()
        .enumerate()
        .map(|(i, page)| {
            Frame::compute(&FrameRequest {
                center_lon: page.center.0,
                center_lat: page.center.1,
                scale: page.scale,
                dpi: page.dpi,
                width: page.width,
                height: page.height,
                zoom: page.zoom,
                style_tile_size,
            })
            .map_err(|e| on_page(i, e.into()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let started = std::time::Instant::now();
    let mut state: Option<Opened> = None;
    let mut resolved: Vec<(Vec<frame_features::Export>, Vec<String>)> = Vec::new();
    for (i, (page, frame)) in pages.iter().zip(&frames).enumerate() {
        let page_started = std::time::Instant::now();
        let counts_before = byte_cache.counts();
        log_frame(frame, style_tile_size, page.scale);
        let opened = match &mut state {
            Some(opened) => opened,
            None => state.insert(open_style(common, &style_text, &doc, byte_cache).await?),
        };
        opened.tiles.set_tile_size(frame.tile_size)?;
        // Every page's features are checked against the graph as soon as
        // there is one, before the first page renders.
        if resolved.is_empty() {
            for (j, p) in pages.iter().enumerate() {
                let r = frame_features::resolve(&opened.graph, &p.features, &p.features_key)
                    .map_err(|e| on_page(j, e.into()))?;
                resolved.push(r);
            }
        }
        let (exports, watched) = &resolved[i];

        let (image, mut captures) = render_frame(&opened.tiles, frame, concurrency, watched)
            .await
            .map_err(|e| on_page(i, e))?;
        let bytes = match page.format {
            OutputFormat::Png => image.encode_png().map_err(|e| e.to_string())?,
            OutputFormat::Webp => pixmap_to_webp(&image).map_err(|e| e.to_string())?,
        };
        std::fs::write(&page.out, &bytes).map_err(|e| on_page(i, e.into()))?;
        tracing::info!("wrote {} ({} bytes)", page.out.display(), bytes.len());
        for export in exports {
            let text = frame_features::feature_collection(
                frame,
                &mut captures,
                export,
                page.features_coords,
            )?;
            std::fs::write(&export.path, &text).map_err(|e| on_page(i, e.into()))?;
            tracing::info!(
                "wrote {} (features of `{}`, {} bytes)",
                export.path.display(),
                export.node,
                text.len()
            );
        }
        if many {
            let (hits, misses) = counts_before;
            let (h, m) = byte_cache.counts();
            tracing::info!(
                "page {}/{}: {} in {:.2}s, {} tiles at z={}, source tiles fetched {}, reused {}",
                i + 1,
                pages.len(),
                page.out.display(),
                page_started.elapsed().as_secs_f64(),
                frame.tile_count(),
                frame.zoom,
                m - misses,
                h - hits,
            );
        }
    }
    if many {
        let (hits, misses) = byte_cache.counts();
        tracing::info!(
            "{} pages in {:.2}s, source tiles fetched {misses}, reused {hits} ({:.1} MB kept)",
            pages.len(),
            started.elapsed().as_secs_f64(),
            byte_cache.held_bytes() as f64 / (1024.0 * 1024.0),
        );
    }
    Ok(())
}

/// Log what a frame settled on: its size, zoom, tile size and how near the
/// scale it came.
fn log_frame(frame: &Frame, style_tile_size: u32, scale: f64) {
    tracing::info!(
        "frame: {}×{} px at z={}, tile-size {} (style: {}), scale 1:{:.1} ({:+.3}% vs 1:{}), \
         shift ({:+.2}, {:+.2}) px, {} tiles ({}..{}, {}..{})",
        frame.width,
        frame.height,
        frame.zoom,
        frame.tile_size,
        style_tile_size,
        frame.scale,
        (frame.scale / scale - 1.0) * 100.0,
        scale,
        frame.shift.0,
        frame.shift.1,
        frame.tile_count(),
        frame.tiles_x.start,
        frame.tiles_x.end,
        frame.tiles_y.start,
        frame.tiles_y.end,
    );
}

/// Render every tile `frame` touches and copy each one's share straight
/// into a frame-sized image.
///
/// Only the frame is ever allocated, never the mosaic of whole tiles
/// around it, and at most `concurrency` rendered tiles are held at once:
/// each is copied in as it finishes and dropped.
///
/// The values of the `watched` nodes are kept from every tile, in the order
/// the tiles finished.
async fn render_frame(
    tiles: &TileRenderer,
    frame: &Frame,
    concurrency: usize,
    watched: &[String],
) -> Result<(Pixmap, Vec<frame_features::TileCapture>), Box<dyn std::error::Error>> {
    let mut image = Pixmap::new(frame.width, frame.height)
        .ok_or_else(|| format!("cannot allocate a {}×{} image", frame.width, frame.height))?;
    let mut captures = Vec::new();
    let xr = frame.tiles_x.clone();
    let coords = frame
        .tiles_y
        .clone()
        .flat_map(move |ty| xr.clone().map(move |tx| (tx, ty)));
    let mut rendered = futures::stream::iter(coords)
        .map(|(tx, ty)| {
            let watched = watched.to_vec();
            async move {
                let tile = CoreTileId::new(frame.zoom, tx, ty);
                let (rgba, values) = tiles
                    .render(tile, move |r, set, mut opts| {
                        opts.format = ezu::renderer::OutputFormat::Rgba;
                        if watched.is_empty() {
                            return r
                                .render_tile_with(tile.z, tile.x, tile.y, set, opts)
                                .map(|px| (px, Vec::new()));
                        }
                        let names: Vec<&str> = watched.iter().map(String::as_str).collect();
                        r.render_tile_with_features(tile.z, tile.x, tile.y, set, opts, &names)
                    })
                    .await?;
                Ok::<_, render::BoxError>((tx, ty, rgba, values))
            }
        })
        .buffer_unordered(concurrency);
    while let Some(done) = rendered.next().await {
        let (tx, ty, rgba, values) = done.map_err(|e| e.to_string())?;
        if !watched.is_empty() {
            captures.push(frame_features::TileCapture {
                x: tx,
                y: ty,
                values,
            });
        }
        let (dx, dy) = frame.tile_offset(tx, ty);
        blit_rgba_into(&mut image, &rgba, dx, dy, frame.tile_size);
    }
    Ok((image, captures))
}

/// Millimetres of paper to whole pixels at `dpi`.
fn mm_to_px(mm: f64, dpi: f64) -> u32 {
    (mm / 25.4 * dpi).round() as u32
}

async fn run_tiles(args: TilesCmd) -> Result<(), Box<dyn std::error::Error>> {
    if args.min_zoom > args.max_zoom {
        return Err("--min-zoom must be ≤ --max-zoom".into());
    }
    let concurrency = args.concurrency.unwrap_or_else(default_concurrency);
    if concurrency == 0 {
        return Err("--concurrency must be ≥ 1".into());
    }
    let opened = open(&args.common).await?;
    let tiles = &opened.tiles;

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
        let out = &args.out;
        let format = args.format;
        let t0 = std::time::Instant::now();
        futures::stream::iter(coords)
            .map(|(tx, ty)| async move {
                let tile = CoreTileId::new(z, tx, ty);
                let bytes = tiles
                    .render(tile, move |r, set, opts| {
                        r.render_tile_with(tile.z, tile.x, tile.y, set, format.with(opts))
                    })
                    .await?;
                let dir = out.join(z.to_string()).join(tx.to_string());
                tokio::fs::create_dir_all(&dir).await?;
                let path = dir.join(format!("{ty}.{}", format.extension()));
                tokio::fs::write(&path, bytes).await?;
                Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
            })
            .buffer_unordered(concurrency)
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| e.to_string())?;
        tracing::info!("z={z}: done in {:.1}s", t0.elapsed().as_secs_f64());
    }
    tracing::info!("wrote {total} tiles → {}", args.out.display());
    Ok(())
}

/// Ceiling on the pixel bytes the intermediate cache retains, from
/// `EZU_CACHE_MB`; `None` keeps the library's own.
///
/// The default is sized for rendering a tile at a time. Bulk runs
/// (`ezu tiles`, `ezu bbox`) re-use world-anchored intermediates across
/// neighbouring tiles, so on a machine with memory to spare, raising it
/// trades RAM for a shorter run.
fn cache_budget_bytes() -> Option<usize> {
    std::env::var("EZU_CACHE_MB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|mb| mb * 1024 * 1024)
}

/// Ceiling on the source tile bytes a run keeps once fetched, from
/// `EZU_TILE_CACHE_MB` (default 256 MB).
fn tile_cache_budget_bytes() -> usize {
    std::env::var("EZU_TILE_CACHE_MB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(256)
        * 1024
        * 1024
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

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Convert a lon/lat bounding box to an inclusive-min, exclusive-max
/// tile range at the given zoom. Latitudes north of ~85.05° are
/// clamped to the Web Mercator domain.
fn bbox_to_tiles(b: BBox, z: u8) -> (std::ops::Range<u32>, std::ops::Range<u32>) {
    let n = 2f64.powi(z as i32);
    let xt = |lng: f64| {
        (ezu::core::coord::lon_to_world_x(lng) * n)
            .floor()
            .clamp(0.0, n - 1.0) as u32
    };
    let yt = |lat: f64| {
        (ezu::core::coord::lat_to_world_y(lat) * n)
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

/// Parse `lng,lat`.
fn parse_lnglat(s: &str) -> Result<(f64, f64), String> {
    let (lng, lat) = s
        .split_once(',')
        .ok_or_else(|| format!("expected `lng,lat`, got `{s}`"))?;
    let num = |p: &str| {
        p.trim()
            .parse::<f64>()
            .map_err(|e| format!("bad number `{p}`: {e}"))
    };
    Ok((num(lng)?, num(lat)?))
}

/// Parse `WIDTHxHEIGHT` in millimetres; fractions allowed.
fn parse_size_mm(s: &str) -> Result<(f64, f64), String> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("`{s}`: expected WIDTHxHEIGHT in millimetres"))?;
    let num = |p: &str| match p.trim().parse::<f64>() {
        Ok(v) if v.is_finite() && v > 0.0 => Ok(v),
        _ => Err(format!(
            "`{s}`: `{p}` is not a positive number of millimetres"
        )),
    };
    Ok((num(w)?, num(h)?))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A network-free style: a translucent polygon, blurred so it reads
    /// across tile edges, over a translucent paper.
    fn style(tile_size: u32) -> String {
        format!(
            r##"{{
              "name": "frame-test",
              "tile-size": {tile_size},
              "pad": 8,
              "sources": {{
                "shapes": {{ "type": "geojson", "data": {{ "type": "FeatureCollection", "features": [
                  {{ "type": "Feature", "properties": {{}}, "geometry": {{ "type": "Polygon",
                    "coordinates": [[[139.698, 35.658], [139.703, 35.659],
                                     [139.701, 35.663], [139.698, 35.658]]] }} }}
                ] }} }}
              }},
              "nodes": {{
                "bg":   {{ "op": "solid", "color": "#fbf6e680" }},
                "feat": {{ "op": "features", "source": "shapes", "layer": "shapes" }},
                "fill": {{ "op": "fill-solid", "features": "@feat", "fill": "#33669980" }},
                "soft": {{ "op": "blur", "input": "@fill", "sigma": 3 }},
                "out":  {{ "op": "blend", "base": "@bg", "over": "@soft" }}
              }},
              "output": "@out"
            }}"##
        )
    }

    fn common(style: &Path) -> CommonArgs {
        CommonArgs {
            style: style.to_string_lossy().into_owned(),
            assets_dir: None,
            pmtiles: None,
            mvt: None,
            overzoom_levels: 4,
            params: Vec::new(),
            strict: false,
        }
    }

    /// `ezu frame` writes exactly the pixels `ezu bbox` draws for the same
    /// tiles, cropped to the frame.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_frame_is_the_matching_crop_of_the_bbox_mosaic() {
        let dir = std::env::temp_dir().join(format!("ezu-frame-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let frame_style = dir.join("frame.json");
        std::fs::write(&frame_style, style(256)).unwrap();

        // 1:5000 at 96 dpi needs tiles of ~188 px at z=17: not the
        // style's 256, and the frame straddles tiles on every side.
        let request = FrameRequest {
            center_lon: 139.70,
            center_lat: 35.66,
            scale: 5000.0,
            dpi: 96.0,
            width: 300,
            height: 200,
            zoom: None,
            style_tile_size: 256,
        };
        let frame = Frame::compute(&request).unwrap();
        assert_ne!(frame.tile_size, 256);
        assert!(frame.tiles_x.len() >= 2 && frame.tiles_y.len() >= 2);

        let frame_out = dir.join("frame.png");
        run_frame(FrameCmd {
            common: common(&frame_style),
            pages: None,
            center: Some((request.center_lon, request.center_lat)),
            scale: Some(request.scale),
            dpi: Some(request.dpi),
            size_mm: None,
            size_px: Some((request.width, request.height)),
            zoom: None,
            out: frame_out.clone(),
            format: None,
            concurrency: Some(2),
            features: Vec::new(),
            features_key: Vec::new(),
            features_coords: frame_features::FeatureCoords::Lonlat,
        })
        .await
        .unwrap();

        // The same tiles through `bbox`, from the style at the frame's
        // tile size, with a box just inside the frame's outer tiles.
        let bbox_style = dir.join("bbox.json");
        std::fs::write(&bbox_style, style(frame.tile_size)).unwrap();
        let n = (1u64 << frame.zoom) as f64;
        let lon = |x: f64| ezu::core::coord::world_x_to_lon(x / n);
        let lat = |y: f64| ezu::core::coord::world_y_to_lat(y / n);
        let bbox_out = dir.join("bbox.png");
        run_bbox(BboxCmd {
            common: common(&bbox_style),
            bbox: BBox {
                min_lng: lon(frame.tiles_x.start as f64 + 0.5),
                max_lng: lon(frame.tiles_x.end as f64 - 0.5),
                max_lat: lat(frame.tiles_y.start as f64 + 0.5),
                min_lat: lat(frame.tiles_y.end as f64 - 0.5),
            },
            zoom: frame.zoom,
            out: bbox_out.clone(),
            format: None,
        })
        .await
        .unwrap();

        let got = Pixmap::load_png(&frame_out).unwrap();
        let mosaic = Pixmap::load_png(&bbox_out).unwrap();
        let t = frame.tile_size;
        assert_eq!(mosaic.width(), frame.tiles_x.len() as u32 * t);
        assert_eq!(mosaic.height(), frame.tiles_y.len() as u32 * t);
        assert_eq!((got.width(), got.height()), (frame.width, frame.height));

        // The frame's corner inside the mosaic.
        let (ox, oy) = frame.tile_offset(frame.tiles_x.start, frame.tiles_y.start);
        let (ox, oy) = ((-ox) as usize, (-oy) as usize);
        let stride = mosaic.width() as usize * 4;
        let row = frame.width as usize * 4;
        for y in 0..frame.height as usize {
            let want = &mosaic.data()[(oy + y) * stride + ox * 4..][..row];
            let have = &got.data()[y * row..][..row];
            assert!(want == have, "row {y} differs");
        }
        // The polygon and its blur reach the frame, so a misplaced tile
        // could not pass by comparing two blank images.
        assert!(got.pixels().windows(2).any(|p| p[0] != p[1]));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_tile_partly_outside_the_destination_copies_only_its_overlap() {
        // A 4×4 tile, every pixel distinct.
        let rgba: Vec<u8> = (0..16u8).flat_map(|i| [i, 0, 0, 255]).collect();
        let at = |x: usize, y: usize| (y * 4 + x) as u8;

        let mut dst = Pixmap::new(5, 3).unwrap();
        blit_rgba_into(&mut dst, &rgba, -2, -1, 4);
        for y in 0..3usize {
            for x in 0..5usize {
                let got = dst.pixel(x as u32, y as u32).unwrap();
                if x < 2 {
                    assert_eq!(got.red(), at(x + 2, y + 1), "({x}, {y})");
                    assert_eq!(got.alpha(), 255);
                } else {
                    assert_eq!(got.alpha(), 0, "({x}, {y}) is outside the tile");
                }
            }
        }

        // Past the far edge, and entirely outside, are both fine.
        let mut dst = Pixmap::new(5, 3).unwrap();
        blit_rgba_into(&mut dst, &rgba, 3, 2, 4);
        assert_eq!(dst.pixel(3, 2).unwrap().red(), at(0, 0));
        assert_eq!(dst.pixel(4, 2).unwrap().red(), at(1, 0));
        blit_rgba_into(&mut dst, &rgba, -4, 0, 4);
        blit_rgba_into(&mut dst, &rgba, 5, 0, 4);
        assert_eq!(dst.pixel(0, 0).unwrap().alpha(), 0);
    }

    /// The renderer hands back pixels demultiplied from its premultiplied
    /// ones, and copying them into an image premultiplies them again. That
    /// gives back every premultiplied pixel exactly, which is what keeps a
    /// page or a mosaic byte-identical to the tiles it is made of.
    #[test]
    fn premultiplying_a_demultiplied_pixel_gives_it_back() {
        for a in 0..=255u8 {
            for c in 0..=a {
                let p = tiny_skia::PremultipliedColorU8::from_rgba(c, c, c, a).unwrap();
                assert_eq!(p.demultiply().premultiply(), p, "({c}, {a})");
            }
        }
    }

    #[test]
    fn frame_size_and_centre_parse() {
        assert_eq!(parse_lnglat("139.7, 35.66"), Ok((139.7, 35.66)));
        assert!(parse_lnglat("139.7").is_err());
        assert_eq!(parse_size_mm("245x174.5"), Ok((245.0, 174.5)));
        assert!(parse_size_mm("245").is_err());
        assert!(parse_size_mm("0x10").is_err());
        // A4 landscape at 200 dpi.
        assert_eq!(
            (mm_to_px(297.0, 200.0), mm_to_px(210.0, 200.0)),
            (2339, 1654)
        );
    }
}
