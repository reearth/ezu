//! Lines convert to the crisp `stroke` op (with dash/cap/join), and
//! `fill-outline-color` becomes a fill-solid `edge`.

use ezu_translate::maplibre::{convert, ConvertOptions};

const STYLE: &str = r##"{
  "version": 8,
  "name": "lines",
  "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
  "layers": [
    { "id": "land", "type": "fill", "source": "s", "source-layer": "earth",
      "paint": { "fill-color": "#eeeeee", "fill-outline-color": "#333333" } },
    { "id": "border", "type": "line", "source": "s", "source-layer": "admin",
      "layout": { "line-cap": "round", "line-join": "round" },
      "paint": { "line-color": "#808080", "line-width": 2, "line-dasharray": [3, 2] } }
  ]
}"##;

#[test]
fn lines_use_stroke_with_dash_and_fill_has_outline() {
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, _) = convert(&style, &ConvertOptions::default()).unwrap();
    let nodes = recipe["nodes"].as_object().unwrap();

    // Line → crisp `stroke` with cap/join and a dash in line widths, as
    // MapLibre measures it.
    let stroke = nodes
        .values()
        .find(|n| n["op"] == "stroke")
        .expect("a stroke node");
    assert_eq!(stroke["width-px"], 2.0);
    assert_eq!(stroke["cap"], "round");
    assert_eq!(stroke["join"], "round");
    assert_eq!(stroke["dasharray"], serde_json::json!([3.0, 2.0]));
    assert_eq!(stroke["dash-unit"], "width");
    // No painterly brush nodes anymore.
    assert!(!nodes
        .values()
        .any(|n| n["op"] == "line" || n["op"] == "brush-solid"));

    // fill-outline-color → fill-solid edge.
    let fill = nodes
        .values()
        .find(|n| n["op"] == "fill-solid")
        .expect("a fill-solid node");
    assert_eq!(fill["edge"], "#333333");

    // Valid Document.
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
    // Without `line-gap-width` the stroke stays a plain centreline stroke.
    assert!(stroke.get("gap-width-px").is_none());
}

const CASING_STYLE: &str = r##"{
  "version": 8,
  "name": "casings",
  "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
  "layers": [
    { "id": "road_casing", "type": "line", "source": "s", "source-layer": "roads",
      "paint": { "line-color": "#ffffff", "line-width": 1.5, "line-gap-width": 6 } },
    { "id": "road_casing_dd", "type": "line", "source": "s", "source-layer": "roads",
      "paint": { "line-color": "#ffffff", "line-width": 1.5,
                 "line-gap-width": ["interpolate", ["linear"], ["zoom"], 12, 2, 16, 10] } }
  ]
}"##;

#[test]
fn line_gap_width_becomes_a_stroke_casing() {
    let style: serde_json::Value = serde_json::from_str(CASING_STYLE).unwrap();
    let (recipe, report) = convert(&style, &ConvertOptions::default()).unwrap();
    let nodes = recipe["nodes"].as_object().unwrap();

    let constant = &nodes["road_casing__stroke"];
    assert_eq!(constant["gap-width-px"], 6.0);
    assert!(constant.get("gap-width-expr").is_none());

    let data_driven = &nodes["road_casing_dd__stroke"];
    assert_eq!(
        data_driven["gap-width-expr"],
        serde_json::json!(["interpolate", ["linear"], ["zoom"], 12, 2, 16, 10])
    );
    assert!(data_driven.get("gap-width-px").is_none());

    assert!(
        !report.warnings.iter().any(|w| w.contains("gap")),
        "gap widths are supported, not warned about: {:?}",
        report.warnings
    );

    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}

const POLYGON_LINE_STYLE: &str = r##"{
  "version": 8,
  "name": "polygon-outline",
  "sources": { "s": { "type": "vector", "tiles": ["https://example.com/{z}/{x}/{y}.pbf"] } },
  "layers": [
    { "id": "block-line", "type": "line", "source": "s", "source-layer": "blocks",
      "paint": { "line-color": "#ff0000", "line-width": 4 } }
  ]
}"##;

#[test]
fn a_line_layer_strokes_the_rings_of_polygon_features() {
    use ezu_features::{Feature, FeatureLayer, Geometry, Polygon};
    use ezu_graph::{build_graph, Cache, CanvasInfo, Evaluator, NoAssets, ParamValues, TileId};
    use ezu_paint::host::TileLoader;

    let style: serde_json::Value = serde_json::from_str(POLYGON_LINE_STYLE).unwrap();
    let (recipe, report) = convert(&style, &ConvertOptions::default()).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    // The stroke reads the polygons' rings, without the edges the tile
    // encoder added where it clipped them.
    let nodes = recipe["nodes"].as_object().unwrap();
    let stroke = &nodes["block-line__stroke"];
    let lines_ref = stroke["features"].as_str().unwrap().trim_start_matches('@');
    assert_eq!(nodes[lines_ref]["op"], "boundary");
    assert_eq!(nodes[lines_ref]["clip-edges"], "drop");

    // A square block from 1024 to 3072 of a 4096 extent: on a 256 px tile
    // its outline runs at 64 and 192 px.
    let mut geometry = Geometry::default();
    geometry.polygons.push(Polygon {
        exterior: vec![(1024, 1024), (3072, 1024), (3072, 3072), (1024, 3072)],
        holes: vec![],
    });
    let layer = FeatureLayer {
        name: "blocks".into(),
        extent: 4096,
        features: vec![Feature {
            id: None,
            geometry,
            properties: Default::default(),
        }],
    };

    let doc = ezu_style::Document::from_json(&serde_json::to_string(&recipe).unwrap()).unwrap();
    let graph = build_graph(&doc, &ezu_paint::nodes::default_registry()).unwrap();
    let cache = Cache::new();
    let tile = TileId { z: 14, x: 0, y: 0 };
    let mut loader = TileLoader::new(&NoAssets, tile);
    loader.bind_features("s.blocks", layer);
    let out = Evaluator::new(&graph, &cache, &loader)
        .render(tile, CanvasInfo::square(256, 0), &ParamValues::new(), 0)
        .unwrap();
    let raster = out.as_raster().expect("a raster");
    let alpha = |x: u32, y: u32| raster.pixels[((y * raster.width + x) * 4 + 3) as usize];

    assert_eq!(alpha(64, 128), 255, "west edge is stroked");
    assert_eq!(alpha(128, 192), 255, "south edge is stroked");
    assert_eq!(alpha(128, 128), 0, "the inside is not filled");
}

/// The `stroke` a single line layer with `paint` converts to, and the
/// conversion's warnings.
fn stroke_for(paint: serde_json::Value) -> (serde_json::Value, Vec<String>) {
    let style = serde_json::json!({
        "version": 8,
        "name": "dashes",
        "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
        "layers": [
            { "id": "path", "type": "line", "source": "s", "source-layer": "paths", "paint": paint }
        ]
    });
    let (recipe, report) = convert(&style, &ConvertOptions::default()).unwrap();
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
    (recipe["nodes"]["path__stroke"].clone(), report.warnings)
}

#[test]
fn dashes_scale_with_an_expression_line_width() {
    // A zoom curve width has no single value to scale the dash by up front,
    // so the dash stays in widths and the node scales it by the width it
    // resolves per feature.
    let width = serde_json::json!(["interpolate", ["linear"], ["zoom"], 10, 1, 16, 4]);
    let (stroke, warnings) = stroke_for(serde_json::json!({
        "line-width": width, "line-dasharray": [2, 1]
    }));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(stroke["width-expr"], width);
    assert_eq!(stroke["dasharray"], serde_json::json!([2.0, 1.0]));
    assert_eq!(stroke["dash-unit"], "width");

    // `["literal", […]]` is the same constant.
    let (stroke, warnings) = stroke_for(serde_json::json!({
        "line-width": 3, "line-dasharray": ["literal", [2, 1]]
    }));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(stroke["dasharray"], serde_json::json!([2.0, 1.0]));
}

#[test]
fn an_expression_line_dasharray_warns_and_draws_solid() {
    let (stroke, warnings) = stroke_for(serde_json::json!({
        "line-width": 2,
        "line-dasharray": ["step", ["zoom"], ["literal", [2, 1]], 14, ["literal", [4, 2]]]
    }));
    assert!(stroke.get("dasharray").is_none());
    assert!(
        warnings.iter().any(|w| w.contains("line-dasharray")),
        "{warnings:?}"
    );
}
