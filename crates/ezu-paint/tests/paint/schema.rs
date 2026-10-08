//! Registry surface: every built-in op shows up in the document schema.

#[test]
fn registry_emits_document_schema_with_all_ops() {
    let registry = ezu_paint::nodes::default_registry();
    let schema = registry.document_schema();
    let s = schema.to_string();
    // Spot-check: every built-in op surfaces in the schema and the
    // document-level structure is there.
    for op in [
        "solid",
        "circle",
        "blur",
        "blend",
        "stack",
        "gradient-linear",
        "gradient-radial",
        "gradient-conic",
        "gradient-diamond",
        "brightness-contrast",
        "hsl",
        "invert",
        "color-to-alpha",
        "features",
        "fill-solid",
        "fill-dabs",
        "line",
        "brush-file",
        "brush-solid",
        "image",
        "dash",
        "wave",
        "stamp",
        "tiling",
        "place",
        "text",
        "text-labels",
        "text-draw",
        "label-placement",
    ] {
        assert!(
            s.contains(&format!("\"const\":\"{op}\"")),
            "missing op `{op}` in schema"
        );
    }
    assert!(s.contains("\"$schema\""));
    assert!(s.contains("\"nodes\""));
    assert!(s.contains("\"output\""));
}

#[test]
fn document_schema_describes_every_source_type() {
    let schema = ezu_paint::nodes::default_registry().document_schema();
    let variants = schema["properties"]["sources"]["additionalProperties"]["oneOf"]
        .as_array()
        .expect("sources oneOf");
    let types: Vec<&str> = variants
        .iter()
        .flat_map(|v| {
            let t = &v["properties"]["type"];
            match t["enum"].as_array() {
                Some(a) => a.iter().filter_map(|x| x.as_str()).collect(),
                None => t["const"].as_str().into_iter().collect::<Vec<_>>(),
            }
        })
        .collect();
    // Every `type` a style's `sources` entry may declare.
    for ty in [
        "brush", "image", "mvt", "pmtiles", "dem", "raster", "geojson", "sprite", "font", "glyphs",
    ] {
        assert!(
            types.contains(&ty),
            "source type `{ty}` missing from schema: {types:?}"
        );
    }
}
