//! Print a histogram of property values in a layer for a given PMTiles tile.
//!
//! Usage: `cargo run --release --example inspect -- <z> <x> <y> [layer]`

use std::collections::BTreeMap;

use bytes::Bytes;
use ezu::core::TileId;
use ezu::features::{mvt, Value};
use pmtiles::{AsyncPmTilesReader, HttpBackend, TileCoord};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: inspect <z> <x> <y> [layer]");
        std::process::exit(2);
    }
    let z: u8 = args[1].parse()?;
    let x: u32 = args[2].parse()?;
    let y: u32 = args[3].parse()?;
    let layer = args.get(4).cloned().unwrap_or_else(|| "roads".to_string());

    let client = reqwest::Client::new();
    let archive =
        AsyncPmTilesReader::new_with_url(client, "https://build.protomaps.com/20260520.pmtiles")
            .await?;
    let bytes: Bytes = archive
        .get_tile_decompressed(TileCoord::new(z, x, y)?)
        .await?
        .ok_or("tile not in archive")?;
    let decoded = mvt::decode(&bytes)?;
    let Some(l) = decoded.layer(&layer) else {
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
    Ok(())
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
