//! Convert the real MapLibre demotiles style and check the recipe both
//! parses as an ezu Document and builds a graph.

use ezu_translate::maplibre::{convert, ConvertOptions};

const DEMOTILES: &str = include_str!("fixtures/demotiles.json");

#[test]
fn converts_and_parses_as_ezu_document() {
    let style: serde_json::Value = serde_json::from_str(DEMOTILES).unwrap();
    let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("conversion");

    // Both symbol layers convert — the line-placed `geolines-label` to a
    // `text` node with `placement: line`, no skipped-text warnings.
    let joined = report.warnings.join("\n");
    assert!(
        !joined.contains("text skipped") && !joined.contains("line placement"),
        "unexpected symbol warnings:\n{joined}"
    );

    // Recipe shape.
    let obj = recipe.as_object().unwrap();
    assert_eq!(obj["tile-size"], 512);
    let sources = obj["sources"].as_object().unwrap();
    assert!(sources.contains_key("maplibre"));
    // The inline-geojson `crimea` source now converts (was previously
    // skipped); its fill layer targets `(crimea, crimea)`.
    assert_eq!(sources["crimea"]["type"], "geojson");
    assert!(sources["crimea"]["data"].is_object());
    let nodes = obj["nodes"].as_object().unwrap();
    // background + fills + lines + blend chain → plenty of nodes.
    assert!(nodes.len() > 10, "unexpectedly few nodes: {}", nodes.len());

    // The `match` on ADM0_A3 should convert to a single `fill-solid` carrying
    // a data-driven `fill-expr` that references the driving property (rather
    // than expanding into N membership-filtered buckets).
    let has_fill_expr = nodes.values().any(|n| {
        n.get("op").and_then(|v| v.as_str()) == Some("fill-solid")
            && n.get("fill-expr")
                .map(|e| serde_json::to_string(e).unwrap().contains("ADM0_A3"))
                .unwrap_or(false)
    });
    assert!(
        has_fill_expr,
        "expected a fill-solid with a fill-expr referencing ADM0_A3"
    );

    // The line-placed geolines label routed through.
    assert_eq!(nodes["geolines-label__text"]["placement"], "line");

    // The crimea geojson layer resolved to a `(crimea, crimea)` features node.
    let has_geojson_layer = nodes.values().any(|n| {
        n.get("op").and_then(|v| v.as_str()) == Some("features")
            && n.get("source").and_then(|v| v.as_str()) == Some("crimea")
            && n.get("layer").and_then(|v| v.as_str()) == Some("crimea")
    });
    assert!(
        has_geojson_layer,
        "expected crimea (crimea, crimea) features node"
    );

    // Must parse + build as a real ezu Document/graph.
    let text = serde_json::to_string(&recipe).unwrap();
    let doc = ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
    assert!(!doc.nodes.is_empty());

    eprintln!(
        "--- conversion report ({} warnings) ---",
        report.warnings.len()
    );
    for w in &report.warnings {
        eprintln!("  - {w}");
    }
}
