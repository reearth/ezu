//! JSON parse → `build_graph` round-trips and the error variants that
//! the builder surfaces.

use super::common::Mock;
use crate::{
    build_graph, take_input_ref, BuildError, BuildGraphError, BuiltNode, Connection, FactoryCtx,
    FactoryError, NodeFactory, NodeRegistry, PortKind, PortSpec,
};

struct SrcFactory(&'static str, PortKind);
impl NodeFactory for SrcFactory {
    fn op_name(&self) -> &'static str {
        self.0
    }
    fn build(
        &self,
        _fields: &serde_json::Map<String, serde_json::Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        Ok(BuiltNode {
            node: Mock::new("src", vec![], self.1).boxed(),
            connections: vec![],
        })
    }
}

struct BlurFactory;
impl NodeFactory for BlurFactory {
    fn op_name(&self) -> &'static str {
        "blur"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, serde_json::Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let sigma = fields
            .get("sigma")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| FactoryError::MissingField("sigma".into()))?;
        let pad = (sigma * 3.0).ceil() as u32;
        Ok(BuiltNode {
            node: Mock::new(
                "blur",
                vec![PortSpec::new("input", &[PortKind::Raster])],
                PortKind::Raster,
            )
            .with_pad_grow(pad)
            .boxed(),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
}

fn test_registry() -> NodeRegistry {
    let mut r = NodeRegistry::new();
    r.register(SrcFactory("image", PortKind::Raster));
    r.register(BlurFactory);
    r
}

#[test]
fn round_trip_parse_and_build() {
    let json = r##"{
      "name": "demo",
      "pad": 8,
      "nodes": {
        "src":  { "op": "image", "src": "x.png" },
        "blur": { "op": "blur", "input": "@src", "sigma": 4 }
      },
      "output": "@blur"
    }"##;
    let doc = ezu_style::Document::from_json(json).unwrap();
    let g = build_graph(&doc, &test_registry()).unwrap();
    assert_eq!(g.len(), 2);
    let pads = g.compute_pad(doc.pad).unwrap();
    let src_ix = g
        .topo_order()
        .iter()
        .find(|&&i| g.node_id(i) == "src")
        .copied()
        .unwrap();
    // blur sigma=4 -> grow 12; src needs at least doc.pad + 12 = 20.
    assert!(pads[src_ix] >= 20);
}

/// A legend entry naming a node that draws a raster is fine, and the
/// declaration comes through the build untouched.
#[test]
fn build_accepts_a_legend_pointing_at_a_drawing_node() {
    let json = r##"{
      "name": "demo",
      "legend": {
        "title": "Median household income",
        "note": "ACS 5-year estimates, classified by natural breaks.",
        "entries": [
          { "label": "under $40,000", "from": "@blur", "properties": { "income": 30000 } },
          { "label": "detail", "from": "@blur", "min-zoom": 8 }
        ]
      },
      "nodes": {
        "src":  { "op": "image", "src": "x.png" },
        "blur": { "op": "blur", "input": "@src", "sigma": 4 }
      },
      "output": "@blur"
    }"##;
    let doc = ezu_style::Document::from_json(json).unwrap();
    build_graph(&doc, &test_registry()).unwrap();
    let legend = doc.legend.as_ref().unwrap();
    assert_eq!(legend.entries.len(), 2);
    assert_eq!(legend.entries[0].from.as_str(), "blur");
    assert_eq!(legend.entries[0].properties["income"], 30000);
    // The zoom-gated entry drops out below its floor.
    assert_eq!(legend.entries_at(4).count(), 1);
    assert_eq!(legend.entries_at(10).count(), 2);
}

#[test]
fn build_rejects_a_legend_entry_naming_no_node() {
    let json = r##"{
      "name": "demo",
      "legend": { "entries": [ { "label": "water", "from": "@nope" } ] },
      "nodes": { "src": { "op": "image", "src": "x.png" } },
      "output": "@src"
    }"##;
    let doc = ezu_style::Document::from_json(json).unwrap();
    match build_graph(&doc, &test_registry()) {
        Err(BuildGraphError::LegendUnknownNode { label, src }) => {
            assert_eq!(label, "water");
            assert_eq!(src, "nope");
        }
        other => panic!("expected LegendUnknownNode, got {other:?}"),
    }
}

/// An entry may only name something that draws. Pointing at a brush or a
/// feature set would give a legend row with no symbol to show.
#[test]
fn build_rejects_a_legend_entry_naming_a_non_raster() {
    let mut reg = test_registry();
    reg.register(SrcFactory("bsrc", PortKind::Brush));
    let json = r##"{
      "name": "demo",
      "legend": { "entries": [ { "label": "water", "from": "@b" } ] },
      "nodes": {
        "b":   { "op": "bsrc" },
        "src": { "op": "image", "src": "x.png" }
      },
      "output": "@src"
    }"##;
    let doc = ezu_style::Document::from_json(json).unwrap();
    match build_graph(&doc, &reg) {
        Err(BuildGraphError::LegendNodeKind { label, got, .. }) => {
            assert_eq!(label, "water");
            assert_eq!(got, PortKind::Brush);
        }
        other => panic!("expected LegendNodeKind, got {other:?}"),
    }
}

#[test]
fn build_unknown_op_errors() {
    let json = r##"{
      "name": "demo",
      "nodes": { "x": { "op": "no-such-op" } },
      "output": "@x"
    }"##;
    let doc = ezu_style::Document::from_json(json).unwrap();
    match build_graph(&doc, &test_registry()) {
        Err(BuildGraphError::UnknownOp { node, op }) => {
            assert_eq!(node, "x");
            assert_eq!(op, "no-such-op");
        }
        other => panic!("expected UnknownOp, got {other:?}"),
    }
}

#[test]
fn build_propagates_type_mismatch() {
    // image returns Raster; register a Brush-emitting "rsrc" and try to
    // feed blur (which expects Raster) with it.
    let mut reg = test_registry();
    reg.register(SrcFactory("rsrc", PortKind::Brush));
    let json = r##"{
      "name": "demo",
      "nodes": {
        "a":    { "op": "rsrc" },
        "blur": { "op": "blur", "input": "@a", "sigma": 1 }
      },
      "output": "@blur"
    }"##;
    let doc = ezu_style::Document::from_json(json).unwrap();
    match build_graph(&doc, &reg) {
        Err(BuildGraphError::Graph(BuildError::TypeMismatch { .. })) => {}
        other => panic!("expected TypeMismatch, got {other:?}"),
    }
}

#[test]
fn build_factory_error_attaches_node_id() {
    let json = r##"{
      "name": "demo",
      "nodes": {
        "src":  { "op": "image", "src": "x.png" },
        "blur": { "op": "blur", "input": "@src" }
      },
      "output": "@blur"
    }"##;
    let doc = ezu_style::Document::from_json(json).unwrap();
    match build_graph(&doc, &test_registry()) {
        Err(BuildGraphError::Factory { node, source }) => {
            assert_eq!(node, "blur");
            assert!(matches!(source, FactoryError::MissingField(ref f) if f == "sigma"));
        }
        other => panic!("expected Factory error, got {other:?}"),
    }
}
