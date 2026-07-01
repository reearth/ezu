//! Convert a MapLibre GL style to an ezu recipe and print it.
//!
//! Usage: cargo run -p ezu-maplibre --example convert -- <style.json> [zoom]
//!
//! Warnings (skipped/approximated layers) go to stderr; the recipe JSON to
//! stdout, so you can redirect it: `... > recipe.json`.

use ezu_maplibre::{convert, ConvertOptions};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: convert <style.json> [zoom]");
    let zoom = args.next().and_then(|z| z.parse::<f64>().ok());

    let text = std::fs::read_to_string(&path).expect("read style");
    let style: serde_json::Value = serde_json::from_str(&text).expect("parse style json");

    let opts = ConvertOptions {
        zoom,
        ..Default::default()
    };
    let (recipe, report) = convert(&style, &opts).expect("convert");

    for w in &report.warnings {
        eprintln!("warn: {w}");
    }
    eprintln!("({} warnings)", report.warnings.len());
    println!("{}", serde_json::to_string_pretty(&recipe).unwrap());
}
