//! Render a small batch of tiles around Tokyo from the public Protomaps
//! daily build over HTTP, using an Ezu Style JSON document.
//!
//! ```text
//! cargo run --release --example tokyo -- [STYLE.json] [BUILD_DATE] [OUT_DIR]
//! ```
//!
//! Defaults:
//! - `STYLE.json` → `crates/ezu/styles/watercolor-basic.json`
//! - `BUILD_DATE` → `20260520`
//! - `OUT_DIR`    → `out/tokyo`
//!
//! Outputs 2×2 PNGs at zoom 13 covering central Tokyo.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ezu::core::TileId;
use ezu::mvt;
use ezu::paint::{self, canvas_from_style, render_style, Brush};
use ezu::pmtiles::PmTilesArchive;
use ezu::style::Style;

const Z: u8 = 13;
const X_RANGE: std::ops::RangeInclusive<u32> = 7276..=7277;
const Y_RANGE: std::ops::RangeInclusive<u32> = 3225..=3226;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let style_path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "crates/ezu/styles/watercolor-basic.json".to_string());
    let date = args.get(2).cloned().unwrap_or_else(|| "20260520".to_string());
    let out_dir = PathBuf::from(
        args.get(3)
            .cloned()
            .unwrap_or_else(|| "out/tokyo".to_string()),
    );
    std::fs::create_dir_all(&out_dir)?;

    let style_json = std::fs::read_to_string(&style_path)?;
    let style = Style::from_json(&style_json)?;
    eprintln!(
        "style: {} v{} (tile={}, pad={}, {} layers)",
        style.name,
        style.version,
        style.tile_size,
        style.pad,
        style.layers.len()
    );

    let brushes = load_brush_bank(Path::new("assets/brushes"))?;
    eprintln!("brush bank: {} brushes", brushes.len());

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
            let tile = TileId::new(Z, x, y);
            eprintln!("rendering {Z}/{x}/{y}");
            let png = render_one(&archive, &style, &brushes, tile).await?;
            let path = out_dir.join(format!("tokyo_z{Z}_x{x}_y{y}.png"));
            std::fs::write(&path, &png)?;
            eprintln!("  wrote {} ({} bytes)", path.display(), png.len());
        }
    }
    Ok(())
}

async fn render_one(
    archive: &PmTilesArchive,
    style: &Style,
    brushes: &HashMap<String, Brush>,
    tile: TileId,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    use std::time::Instant;

    let mut canvas = canvas_from_style(style);

    let t0 = Instant::now();
    let mvt_bytes = match archive.get_tile(tile).await? {
        Some(b) => b,
        None => {
            eprintln!("  (tile not found, returning paper-only)");
            return Ok(paint::encode_png(&canvas)?);
        }
    };
    let t_fetch = t0.elapsed();

    let t1 = Instant::now();
    let decoded = mvt::decode(&mvt_bytes)?;
    let t_decode = t1.elapsed();

    let resolver = |name: &str| -> Option<&Brush> {
        let key = name.strip_prefix('@').unwrap_or(name);
        brushes.get(key)
    };

    let t2 = Instant::now();
    render_style(&mut canvas, style, &decoded, tile, &resolver)?;
    let t_paint = t2.elapsed();

    let t3 = Instant::now();
    let png = paint::encode_png(&canvas)?;
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

/// Load every `*.myb` file from `dir` into a brush bank keyed by file stem.
fn load_brush_bank(dir: &Path) -> Result<HashMap<String, Brush>, Box<dyn std::error::Error>> {
    let mut bank = HashMap::new();
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
        bank.insert(name, brush);
    }
    Ok(bank)
}
