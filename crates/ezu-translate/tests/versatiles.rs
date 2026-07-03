//! Convert the VersaTiles "Colorful" style — a full 324-layer OSM basemap
//! over the Shortbread schema — as a stress test of the converter against a
//! real-world modern style (expression-form filters, hundreds of lines).

use ezu_translate::maplibre::{convert, ConvertOptions};

const STYLE: &str = include_str!("fixtures/versatiles-colorful.json");

#[test]
fn converts_full_osm_style_to_valid_document() {
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("conversion");

    // Big real style → a large recipe that still builds as an ezu Document.
    let text = serde_json::to_string(&recipe).unwrap();
    let doc = ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
    assert!(
        doc.nodes.len() > 300,
        "unexpectedly small: {}",
        doc.nodes.len()
    );

    // Expression-form filters must survive conversion (not be silently
    // dropped): the style uses `["in", ["get", "kind"], ["literal", [...]]]`
    // and other expression filters heavily. These now pass through verbatim
    // as raw `filter-expr` (evaluated by ezu-paint via `maplibre-expr` with
    // full fidelity) rather than the lossy structured translation, so many
    // features nodes should carry a `filter-expr`.
    let nodes = recipe["nodes"].as_object().unwrap();
    let expr_filters = nodes
        .values()
        .filter(|n| n["op"] == "features")
        .filter(|n| n.get("filter-expr").map(|e| e.is_array()).unwrap_or(false))
        .count();
    assert!(
        expr_filters > 5,
        "expected expression-form filters to pass through as `filter-expr`, got {expr_filters}"
    );

    // The residual should be the known-unsupported set only.
    let unexpected: Vec<_> = report
        .warnings
        .iter()
        .filter(|w| {
            !(w.contains("symbol")
                || w.contains("icon-image")
                // Data-driven icon-size/-rotate/-opacity have no `*-expr` on
                // the `stamp` node, so they're dropped with a warning.
                || w.contains("on `stamp`")
                || w.contains("dasharray")
                // Collision knobs ezu doesn't model yet (icon collision,
                // text/icon pairing, cooperative overlap).
                || w.contains("text-optional")
                || w.contains("overlap")
                || w.contains("ignore-placement")
                || w.contains("`has`")
                || w.contains("`!has`")
                || w.contains("`any`"))
        })
        .collect();
    assert!(
        unexpected.is_empty(),
        "unexpected warnings:\n{unexpected:#?}"
    );
}
