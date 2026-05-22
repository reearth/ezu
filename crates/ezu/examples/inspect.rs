//! Inspect a PMTiles tile: per-key property histogram, plus an optional
//! per-feature dump with geometry bboxes / areas.
//!
//! Usage:
//! ```text
//! cargo run --release --features="features http" --example inspect -- \
//!     <z> <x> <y> [layer] [--each]
//! ```
//!
//! Defaults: `layer=roads`. Pass `--each` (or `-e`) to list every
//! feature with kind / name / geometry summary — useful for diagnosing
//! polygon overlap.

use std::collections::{BTreeMap, HashMap};

use bytes::Bytes;
use ezu::features::{mvt, Feature, Value};
use pmtiles::{AsyncPmTilesReader, TileCoord};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let argv: Vec<String> = std::env::args().collect();
    let each = argv.iter().any(|a| a == "--each" || a == "-e");
    let pos: Vec<&String> = argv
        .iter()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    if pos.len() < 3 {
        eprintln!("usage: inspect <z> <x> <y> [layer] [--each]");
        std::process::exit(2);
    }
    let z: u8 = pos[0].parse()?;
    let x: u32 = pos[1].parse()?;
    let y: u32 = pos[2].parse()?;
    let layer = pos.get(3).map(|s| s.as_str()).unwrap_or("roads");

    let client = reqwest::Client::new();
    let archive =
        AsyncPmTilesReader::new_with_url(client, "https://build.protomaps.com/20260520.pmtiles")
            .await?;
    let bytes: Bytes = archive
        .get_tile_decompressed(TileCoord::new(z, x, y)?)
        .await?
        .ok_or("tile not in archive")?;
    let decoded = mvt::decode(&bytes)?;
    let Some(l) = decoded.layer(layer) else {
        eprintln!(
            "layer {layer} not in tile; available: {:?}",
            decoded.layers.iter().map(|l| &l.name).collect::<Vec<_>>()
        );
        std::process::exit(1);
    };
    eprintln!(
        "{}: {} features, extent={}",
        l.name,
        l.features.len(),
        l.extent
    );

    // Property histogram.
    let mut per_key: BTreeMap<&str, BTreeMap<String, usize>> = BTreeMap::new();
    for f in &l.features {
        for (k, v) in &f.properties {
            *per_key
                .entry(k.as_str())
                .or_default()
                .entry(value_label(v))
                .or_insert(0) += 1;
        }
    }
    for (k, counts) in &per_key {
        eprintln!("\n  {} ({} distinct):", k, counts.len());
        let mut sorted: Vec<_> = counts.iter().collect();
        sorted.sort_by_key(|(_, &c)| std::cmp::Reverse(c));
        for (v, c) in sorted.iter().take(20) {
            eprintln!("    {:>6}  {}", c, v);
        }
    }

    if !each {
        return Ok(());
    }

    // Per-feature dump.
    eprintln!("\nfeatures:");
    let ext = l.extent as f64;
    for (i, f) in l.features.iter().enumerate() {
        let kind = prop(&f.properties, "kind");
        let detail = prop(&f.properties, "kind_detail");
        let name = prop(&f.properties, "name");
        let mz = prop(&f.properties, "min_zoom");
        let geom = summarize_geometry(f, ext);
        eprintln!(
            "  #{:>3} kind={:<14} detail={:<10} z>={:<3} {}  name={}",
            i, kind, detail, mz, geom, name
        );
    }
    Ok(())
}

fn prop(p: &HashMap<String, Value>, key: &str) -> String {
    match p.get(key) {
        None => "-".into(),
        Some(v) => value_label(v),
    }
}

fn value_label(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Int(n) | Value::SInt(n) => n.to_string(),
        Value::UInt(n) => n.to_string(),
        Value::Float(n) => n.to_string(),
        Value::Double(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "(null)".into(),
    }
}

fn summarize_geometry(f: &Feature, ext: f64) -> String {
    let g = &f.geometry;
    let mut parts: Vec<String> = Vec::new();
    if !g.points.is_empty() {
        parts.push(format!("points={}", g.points.len()));
    }
    if !g.lines.is_empty() {
        let verts: usize = g.lines.iter().map(|l| l.len()).sum();
        parts.push(format!("lines={} verts={}", g.lines.len(), verts));
    }
    if !g.polygons.is_empty() {
        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;
        let mut rings = 0;
        let mut verts = 0;
        let mut area = 0.0_f64;
        for p in &g.polygons {
            rings += 1 + p.holes.len();
            verts += p.exterior.len() + p.holes.iter().map(|h| h.len()).sum::<usize>();
            for &(px, py) in &p.exterior {
                min_x = min_x.min(px);
                min_y = min_y.min(py);
                max_x = max_x.max(px);
                max_y = max_y.max(py);
            }
            area += shoelace(&p.exterior).abs();
            for h in &p.holes {
                area -= shoelace(h).abs();
            }
        }
        parts.push(format!(
            "polys={} rings={} verts={} bbox=[{:>3.0},{:>3.0}..{:>3.0},{:>3.0}]% area={:>5.2}%",
            g.polygons.len(),
            rings,
            verts,
            100.0 * min_x as f64 / ext,
            100.0 * min_y as f64 / ext,
            100.0 * max_x as f64 / ext,
            100.0 * max_y as f64 / ext,
            100.0 * area / (ext * ext),
        ));
    }
    if parts.is_empty() {
        "(no geometry)".into()
    } else {
        parts.join(" ")
    }
}

fn shoelace(ring: &[(i32, i32)]) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    let mut s = 0.0;
    for i in 0..ring.len() {
        let (x1, y1) = ring[i];
        let (x2, y2) = ring[(i + 1) % ring.len()];
        s += (x1 as f64) * (y2 as f64) - (x2 as f64) * (y1 as f64);
    }
    s * 0.5
}
