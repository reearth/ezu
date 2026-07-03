//! Convert a MapLibre GL style to an ezu recipe and print it.
//!
//! Usage: cargo run -p ezu-translate --example convert -- <style.json> [--keep-hidden]
//!
//! Warnings (skipped/approximated layers) go to stderr; the recipe JSON to
//! stdout, so you can redirect it: `... > recipe.json`. The recipe is
//! zoom-independent — zoom/data functions are emitted as raw `*-expr` and
//! evaluated per tile — so there's nothing to bake at a fixed zoom.

use ezu_translate::maplibre::{convert, ConvertOptions};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let keep_hidden = args.iter().any(|a| a == "--keep-hidden");
    let mut pos = args.iter().filter(|a| !a.starts_with("--"));
    let path = pos
        .next()
        .expect("usage: convert <style.json> [--keep-hidden]");

    let text = std::fs::read_to_string(path).expect("read style");
    let style: serde_json::Value = serde_json::from_str(&text).expect("parse style json");

    let opts = ConvertOptions {
        keep_hidden,
        ..Default::default()
    };
    let (recipe, report) = convert(&style, &opts).expect("convert");

    for w in &report.warnings {
        eprintln!("warn: {w}");
    }
    eprintln!("({} warnings)", report.warnings.len());
    println!("{}", serde_json::to_string_pretty(&recipe).unwrap());
}
