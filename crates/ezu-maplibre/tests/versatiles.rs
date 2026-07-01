//! Convert the VersaTiles "Colorful" style — a full 324-layer OSM basemap
//! over the Shortbread schema — as a stress test of the converter against a
//! real-world modern style (expression-form filters, hundreds of lines).

use ezu_maplibre::{convert, ConvertOptions};

const STYLE: &str = include_str!("fixtures/versatiles-colorful.json");

#[test]
fn converts_full_osm_style_to_valid_document() {
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = ConvertOptions {
        zoom: Some(14.0),
        ..Default::default()
    };
    let (recipe, report) = convert(&style, &opts).expect("conversion");

    // Big real style → a large recipe that still builds as an ezu Document.
    let text = serde_json::to_string(&recipe).unwrap();
    let doc = ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
    assert!(
        doc.nodes.len() > 300,
        "unexpectedly small: {}",
        doc.nodes.len()
    );

    // Expression-form filters must convert (not be silently dropped): the
    // style uses `["in", ["get", "kind"], ["literal", [...]]]` heavily, so
    // membership-array filters should appear on features nodes.
    let nodes = recipe["nodes"].as_object().unwrap();
    let membership_filters = nodes
        .values()
        .filter(|n| n["op"] == "features")
        .filter_map(|n| n.get("filter").and_then(|f| f.as_object()))
        .filter(|f| f.values().any(|v| v.is_array()))
        .count();
    assert!(
        membership_filters > 5,
        "expected expression-form `in` filters to convert to membership arrays, got {membership_filters}"
    );

    // The residual should be the known-unsupported set only.
    let unexpected: Vec<_> = report
        .warnings
        .iter()
        .filter(|w| {
            !(w.contains("symbol")
                || w.contains("dasharray")
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
