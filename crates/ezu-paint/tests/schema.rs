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
    ] {
        assert!(s.contains(&format!("\"const\":\"{op}\"")), "missing op `{op}` in schema");
    }
    assert!(s.contains("\"$schema\""));
    assert!(s.contains("\"nodes\""));
    assert!(s.contains("\"output\""));
}
