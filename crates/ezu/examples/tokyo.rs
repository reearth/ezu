//! Render a small batch of tiles around Tokyo from the public Protomaps
//! daily build over HTTP, using an Ezu Style JSON document.
//!
//! ```text
//! cargo run --release --features parallel --example tokyo -- [STYLE.json] [BUILD_DATE] [OUT_DIR]
//! ```
//!
//! Defaults:
//! - `STYLE.json`  → `crates/ezu/examples/styles/watercolor-basic.json`
//! - `BUILD_DATE`  → `20260520`
//! - `OUT_DIR`     → `out/tokyo`
//!
//! Outputs 2×2 PNGs at zoom 13 covering central Tokyo. Tiles are
//! fetched and painted concurrently on the tokio multi-thread runtime,
//! and within each tile the node DAG is evaluated in parallel via Rayon.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use ezu::core::TileId as CoreTileId;
use ezu::features::mvt;
use ezu::graph::{
    build_graph, Cache, CanvasInfo, Evaluator, Graph, ParamValues, PortValue, TileId,
};
use ezu::paint::host::{raster_to_png, BrushBankLoader, TileLoader};
use ezu::paint::nodes::default_registry;
use ezu::style::Document;
use futures::future::try_join_all;
use pmtiles::{AsyncPmTilesReader, HttpBackend, TileCoord};

const Z: u8 = 13;
const X_RANGE: std::ops::RangeInclusive<u32> = 7276..=7277;
const Y_RANGE: std::ops::RangeInclusive<u32> = 3225..=3226;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let style_path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "crates/ezu/examples/styles/watercolor-basic.json".to_string());
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

    // 2. Set up the asset loader. Pre-resolves every entry in
    //    `doc.assets` (URL or local file under `assets/brushes`) into
    //    a `BrushBankLoader`; unbound disk lookups fall back to the
    //    same directory.
    let base_dir = Path::new("assets/brushes");
    let mut loader = BrushBankLoader::new()
        .with_dir(base_dir.to_path_buf())
        .with_images_dir(base_dir.to_path_buf());
    ezu::paint::host::prefetch_doc_assets(&doc, base_dir, &mut loader).await?;
    let loader = Arc::new(loader);
    eprintln!(
        "brush bank: {} brushes, {} images",
        loader.bank.len(),
        loader.images.len()
    );

    // 3. Build the graph from the document + registry. One-time cost.
    let registry = default_registry();
    let graph = Arc::new(build_graph(&doc, &registry)?);
    eprintln!(
        "graph: {} nodes, output `{}`",
        graph.len(),
        graph.node_id(graph.output())
    );

    // Per-style cache shared across tiles.
    let cache = Arc::new(Cache::new());
    let canvas = CanvasInfo {
        tile_size: doc.tile_size,
        pad: doc.pad,
    };

    // 4. Fetch + render every tile concurrently.
    let url = format!("https://build.protomaps.com/{date}.pmtiles");
    eprintln!("opening {url}");
    let archive = Arc::new(PmTilesArchive::open_url(&url).await?);
    let header = archive.header();
    eprintln!(
        "header: min_zoom={} max_zoom={} tile_type={:?}",
        header.min_zoom, header.max_zoom, header.tile_type
    );

    let t_total = std::time::Instant::now();
    let mut tasks = Vec::new();
    for y in Y_RANGE {
        for x in X_RANGE {
            let tile = CoreTileId::new(Z, x, y);
            let archive = Arc::clone(&archive);
            let graph = Arc::clone(&graph);
            let cache = Arc::clone(&cache);
            let loader = Arc::clone(&loader);
            let out_dir = out_dir.clone();
            tasks.push(tokio::spawn(async move {
                eprintln!("rendering {Z}/{x}/{y}");
                let png = render_one(archive, graph, cache, loader, canvas, tile).await?;
                let path = out_dir.join(format!("tokyo_z{Z}_x{x}_y{y}.png"));
                std::fs::write(&path, &png)?;
                eprintln!("  wrote {} ({} bytes)", path.display(), png.len());
                Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
            }));
        }
    }
    try_join_all(tasks).await?;
    eprintln!(
        "wall-clock: {:>6.1}ms",
        t_total.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}

async fn render_one(
    archive: Arc<PmTilesArchive>,
    graph: Arc<Graph>,
    cache: Arc<Cache>,
    loader: Arc<BrushBankLoader>,
    canvas: CanvasInfo,
    tile: CoreTileId,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    use std::time::Instant;

    let t0 = Instant::now();
    let mvt_bytes = archive.get_tile(tile).await?;
    let t_fetch = t0.elapsed();

    // Move the CPU-heavy work off the tokio reactor; Arcs travel into
    // the blocking task by value.
    let (png, t_decode, t_paint, t_encode) = tokio::task::spawn_blocking(move || {
        let tile_id = TileId {
            z: tile.z,
            x: tile.x,
            y: tile.y,
        };
        let t1 = Instant::now();
        let mut tile_loader = TileLoader::new(loader.as_ref(), tile_id);
        if let Some(bytes) = mvt_bytes {
            tile_loader.bind_mvt(mvt::decode(&bytes)?);
        }
        let t_decode = t1.elapsed();

        let t2 = Instant::now();
        let ev = Evaluator::new(&graph, &cache, &tile_loader);
        let out = ev.render_parallel(tile_id, canvas, &ParamValues::new(), tile_seed(tile))?;
        let raster = match out {
            PortValue::Raster(r) => r,
            other => {
                return Err::<_, Box<dyn std::error::Error + Send + Sync>>(
                    format!("expected Raster output, got {:?}", other.kind()).into(),
                )
            }
        };
        let t_paint = t2.elapsed();

        let t3 = Instant::now();
        let png = raster_to_png(&raster, canvas.tile_size, canvas.pad)?;
        let t_encode = t3.elapsed();

        Ok((png, t_decode, t_paint, t_encode))
    })
    .await??;

    eprintln!(
        "  {}/{}/{}: fetch={:>7.1}ms decode={:>5.1}ms paint={:>6.1}ms png={:>5.1}ms total={:>6.1}ms",
        tile.z, tile.x, tile.y,
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

/// Minimal PMTiles HTTP wrapper used by this example. Inlined so the
/// example stays self-contained — `ezu` itself does not own remote
/// fetch.
struct PmTilesArchive {
    inner: AsyncPmTilesReader<HttpBackend>,
}

impl PmTilesArchive {
    async fn open_url(url: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let client = reqwest::Client::new();
        let inner = AsyncPmTilesReader::new_with_url(client, url).await?;
        Ok(Self { inner })
    }
    fn header(&self) -> &pmtiles::Header {
        self.inner.get_header()
    }
    async fn get_tile(
        &self,
        tile: CoreTileId,
    ) -> Result<Option<Bytes>, Box<dyn std::error::Error + Send + Sync>> {
        let coord = TileCoord::new(tile.z, tile.x, tile.y)?;
        Ok(self.inner.get_tile_decompressed(coord).await?)
    }
}
