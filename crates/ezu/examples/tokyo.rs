//! Render a small batch of tiles around Tokyo from the public Protomaps
//! daily build over HTTP, using an Ezu **Style** JSON document.
//!
//! ```text
//! cargo run --release --example tokyo -- [STYLE.json] [BUILD_DATE] [OUT_DIR]
//! ```
//!
//! Defaults:
//! - `STYLE.json` → `crates/ezu/examples/watercolor-basic.json`
//! - `BUILD_DATE`    → `20260520`
//! - `OUT_DIR`       → `out/tokyo`
//!
//! Outputs 2×2 PNGs at zoom 13 covering central Tokyo.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ezu::core::TileId as CoreTileId;
use ezu::graph::{
    build_graph, Cache, CanvasInfo, Evaluator, OpaqueValue, ParamValues, PortValue, TileId,
};
use ezu::mvt;
use ezu::paint::host::{raster_to_png, BrushBankLoader};
use ezu::paint::nodes::default_registry;
use ezu::pmtiles::PmTilesArchive;
use ezu::style::Document;

const Z: u8 = 13;
const X_RANGE: std::ops::RangeInclusive<u32> = 7276..=7277;
const Y_RANGE: std::ops::RangeInclusive<u32> = 3225..=3226;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let style_path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "crates/ezu/examples/watercolor-basic.json".to_string());
    let date = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "20260520".to_string());
    let out_dir = PathBuf::from(
        args.get(3)
            .cloned()
            .unwrap_or_else(|| "out/tokyo".to_string()),
    );
    std::fs::create_dir_all(&out_dir)?;

    // 1. Parse the style.
    let style_json = std::fs::read_to_string(&style_path)?;
    let doc = Document::from_json(&style_json)?;
    eprintln!(
        "style: {} v{} (tile={}, pad={}, {} nodes)",
        doc.name,
        doc.version,
        doc.tile_size,
        doc.pad,
        doc.nodes.len()
    );

    // 2. Set up the asset loader (brush bank).
    let loader = build_brush_loader(Path::new("assets/brushes"))?;
    eprintln!("brush bank: {} brushes", loader.bank.len());

    // 3. Build the graph from the document + registry. One-time cost.
    let registry = default_registry();
    let graph = build_graph(&doc, &registry)?;
    eprintln!("graph: {} nodes, output `{}`", graph.len(), graph.node_id(graph.output()));

    // Per-style cache shared across tiles.
    let cache = Cache::new();
    let canvas = CanvasInfo {
        tile_size: doc.tile_size,
        pad: doc.pad,
    };

    // 4. Fetch + render each tile.
    let url = format!("https://build.protomaps.com/{date}.pmtiles");
    eprintln!("opening {url}");
    let archive = PmTilesArchive::open_url(&url).await?;
    let header = archive.header();
    eprintln!(
        "header: min_zoom={} max_zoom={} tile_type={:?}",
        header.min_zoom, header.max_zoom, header.tile_type
    );

    for y in Y_RANGE {
        for x in X_RANGE {
            let tile = CoreTileId::new(Z, x, y);
            eprintln!("rendering {Z}/{x}/{y}");
            let png = render_one(&archive, &graph, &cache, &loader, &canvas, tile).await?;
            let path = out_dir.join(format!("tokyo_z{Z}_x{x}_y{y}.png"));
            std::fs::write(&path, &png)?;
            eprintln!("  wrote {} ({} bytes)", path.display(), png.len());
        }
    }
    Ok(())
}

async fn render_one(
    archive: &PmTilesArchive,
    graph: &ezu::graph::Graph,
    cache: &Cache,
    loader: &BrushBankLoader,
    canvas: &CanvasInfo,
    tile: CoreTileId,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    use std::time::Instant;

    let t0 = Instant::now();
    let mvt_bytes = archive.get_tile(tile).await?;
    let t_fetch = t0.elapsed();

    let t1 = Instant::now();
    let tile_data: Option<OpaqueValue> = match mvt_bytes {
        Some(bytes) => Some(Arc::new(mvt::decode(&bytes)?) as OpaqueValue),
        None => {
            eprintln!("  (tile not found, will render paper only)");
            None
        }
    };
    let t_decode = t1.elapsed();

    let t2 = Instant::now();
    let ev = Evaluator::new(graph, cache, loader);
    let out = ev.render_with_tile_data(
        TileId {
            z: tile.z,
            x: tile.x,
            y: tile.y,
        },
        *canvas,
        &ParamValues::new(),
        tile_seed(tile),
        tile_data.as_ref(),
    )?;
    let raster = match out {
        PortValue::Raster(r) => r,
        other => return Err(format!("expected Raster output, got {:?}", other.kind()).into()),
    };
    let t_paint = t2.elapsed();

    let t3 = Instant::now();
    let png = raster_to_png(&raster, canvas.tile_size, canvas.pad)?;
    let t_encode = t3.elapsed();

    eprintln!(
        "  timings: fetch={:>7.1}ms decode={:>5.1}ms paint={:>6.1}ms png={:>5.1}ms total={:>6.1}ms",
        t_fetch.as_secs_f64() * 1000.0,
        t_decode.as_secs_f64() * 1000.0,
        t_paint.as_secs_f64() * 1000.0,
        t_encode.as_secs_f64() * 1000.0,
        t0.elapsed().as_secs_f64() * 1000.0,
    );

    Ok(png)
}

fn tile_seed(tile: CoreTileId) -> u64 {
    let mut s = 0u64;
    s = s.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(tile.z as u64);
    s = s.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(tile.x as u64);
    s = s.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(tile.y as u64);
    s
}

/// Load every `*.myb` file from `dir` into a brush bank keyed by file stem.
fn build_brush_loader(dir: &Path) -> Result<BrushBankLoader, Box<dyn std::error::Error>> {
    let mut loader = BrushBankLoader::new().with_dir(dir.to_path_buf());
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("myb") {
            continue;
        }
        let json = std::fs::read_to_string(&path)?;
        let brush = hokusai::myb::from_str(&json)?;
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("bad brush filename")?
            .to_string();
        loader.insert(name, brush);
    }
    Ok(loader)
}
