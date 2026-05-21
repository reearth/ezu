//! M0 tests: topology, type checking, pad propagation. Uses mock nodes
//! that carry only structural metadata — no evaluation yet.

use std::sync::Arc;

use xxhash_rust::xxh3::Xxh3;

use crate::{
    build_graph, BuildError, BuildGraphError, BuiltNode, Cache, CanvasInfo, Connection,
    CoordSpace, EvalCtx, EvalError, Evaluator, FactoryCtx, FactoryError, GraphBuilder, MaskBuf,
    NoAssets, Node, NodeFactory, NodeRegistry, ParamValues, PortKind, PortSpec, PortValue,
    RasterBuf, TileId,
};

/// A mock node with configurable ports and pad growth.
struct Mock {
    op: &'static str,
    inputs: Vec<PortSpec>,
    output: PortKind,
    pad_grow: u32,
    space: CoordSpace,
}

impl Mock {
    fn new(op: &'static str, inputs: Vec<PortSpec>, output: PortKind) -> Self {
        Self {
            op,
            inputs,
            output,
            pad_grow: 0,
            space: CoordSpace::Inherit,
        }
    }
    fn with_pad_grow(mut self, g: u32) -> Self {
        self.pad_grow = g;
        self
    }
    fn boxed(self) -> Box<dyn Node> {
        Box::new(self)
    }
}

impl Node for Mock {
    fn op_name(&self) -> &'static str {
        self.op
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.inputs
    }
    fn output(&self) -> PortKind {
        self.output
    }
    fn coord_space(&self) -> CoordSpace {
        self.space
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        downstream + self.pad_grow
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        _inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let size = ctx.canvas.padded_size();
        Ok(match self.output {
            PortKind::Raster => PortValue::Raster(Arc::new(RasterBuf::filled(
                size, size, [0, 0, 0, 255],
            ))),
            PortKind::Mask => PortValue::Mask(Arc::new(MaskBuf::filled(size, size, 0.0))),
            other => panic!("mock node has no default output for {other:?}"),
        })
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(self.op.as_bytes());
        h.update(&self.pad_grow.to_le_bytes());
    }
}

fn src(kind: PortKind) -> Box<dyn Node> {
    Mock::new("src", vec![], kind).boxed()
}

fn passthrough(input: PortKind, output: PortKind) -> Box<dyn Node> {
    Mock::new("pass", vec![PortSpec::new("input", input)], output).boxed()
}

#[test]
fn linear_chain_topo() {
    let mut b = GraphBuilder::new();
    b.add_node("a", src(PortKind::Mask))
        .add_node("b", passthrough(PortKind::Mask, PortKind::Mask))
        .add_node("c", passthrough(PortKind::Mask, PortKind::Raster))
        .connect("a", "b", "input")
        .connect("b", "c", "input")
        .set_output("c");
    let g = b.build().unwrap();
    let order: Vec<_> = g.topo_order().iter().map(|&i| g.node_id(i)).collect();
    assert_eq!(order, vec!["a", "b", "c"]);
}

#[test]
fn diamond_topo() {
    // a -> b, a -> c, b+c -> d
    let merge = Mock::new(
        "merge",
        vec![
            PortSpec::new("left", PortKind::Mask),
            PortSpec::new("right", PortKind::Mask),
        ],
        PortKind::Raster,
    )
    .boxed();

    let mut b = GraphBuilder::new();
    b.add_node("a", src(PortKind::Mask))
        .add_node("b", passthrough(PortKind::Mask, PortKind::Mask))
        .add_node("c", passthrough(PortKind::Mask, PortKind::Mask))
        .add_node("d", merge)
        .connect("a", "b", "input")
        .connect("a", "c", "input")
        .connect("b", "d", "left")
        .connect("c", "d", "right")
        .set_output("d");
    let g = b.build().unwrap();
    let order: Vec<_> = g.topo_order().iter().map(|&i| g.node_id(i)).collect();
    assert_eq!(order[0], "a");
    assert_eq!(order[3], "d");
    // b and c can appear in either order, both before d.
    assert!(order.contains(&"b"));
    assert!(order.contains(&"c"));
}

#[test]
fn type_mismatch_is_rejected() {
    let mut b = GraphBuilder::new();
    b.add_node("a", src(PortKind::Raster))
        .add_node("b", passthrough(PortKind::Mask, PortKind::Mask))
        .connect("a", "b", "input")
        .set_output("b");
    match b.build() {
        Err(BuildError::TypeMismatch {
            expected, got, ..
        }) => {
            assert_eq!(expected, PortKind::Mask);
            assert_eq!(got, PortKind::Raster);
        }
        other => panic!("expected TypeMismatch, got {other:?}"),
    }
}

#[test]
fn missing_required_port_is_rejected() {
    let mut b = GraphBuilder::new();
    b.add_node("b", passthrough(PortKind::Mask, PortKind::Mask))
        .set_output("b");
    match b.build() {
        Err(BuildError::MissingInput { node, port }) => {
            assert_eq!(node, "b");
            assert_eq!(port, "input");
        }
        other => panic!("expected MissingInput, got {other:?}"),
    }
}

#[test]
fn optional_port_may_be_unconnected() {
    let opt = Mock::new(
        "opt",
        vec![
            PortSpec::new("input", PortKind::Mask),
            PortSpec::new("extra", PortKind::Mask).optional(),
        ],
        PortKind::Mask,
    )
    .boxed();
    let mut b = GraphBuilder::new();
    b.add_node("a", src(PortKind::Mask))
        .add_node("o", opt)
        .connect("a", "o", "input")
        .set_output("o");
    let g = b.build().unwrap();
    assert_eq!(g.len(), 2);
}

#[test]
fn cycle_is_detected() {
    // a -> b -> a; pure cycle (no entry node) is unreachable from output,
    // but is still rejected.
    let mut b = GraphBuilder::new();
    b.add_node("a", passthrough(PortKind::Mask, PortKind::Mask))
        .add_node("b", passthrough(PortKind::Mask, PortKind::Mask))
        .connect("a", "b", "input")
        .connect("b", "a", "input")
        .set_output("b");
    match b.build() {
        Err(BuildError::Cycle(_)) => {}
        other => panic!("expected Cycle, got {other:?}"),
    }
}

#[test]
fn unknown_port_name_is_rejected() {
    let mut b = GraphBuilder::new();
    b.add_node("a", src(PortKind::Mask))
        .add_node("b", passthrough(PortKind::Mask, PortKind::Mask))
        .connect("a", "b", "nope")
        .set_output("b");
    match b.build() {
        Err(BuildError::UnknownPort { node, port }) => {
            assert_eq!(node, "b");
            assert_eq!(port, "nope");
        }
        other => panic!("expected UnknownPort, got {other:?}"),
    }
}

#[test]
fn duplicate_edge_is_rejected() {
    let mut b = GraphBuilder::new();
    b.add_node("a", src(PortKind::Mask))
        .add_node("c", src(PortKind::Mask))
        .add_node("b", passthrough(PortKind::Mask, PortKind::Mask))
        .connect("a", "b", "input")
        .connect("c", "b", "input")
        .set_output("b");
    match b.build() {
        Err(BuildError::DuplicateEdge { .. }) => {}
        other => panic!("expected DuplicateEdge, got {other:?}"),
    }
}

#[test]
fn pad_propagates_upstream_through_blur() {
    // src -> blur(grow=24) -> out(passthrough)
    let mut b = GraphBuilder::new();
    b.add_node("src", src(PortKind::Mask))
        .add_node(
            "blur",
            Mock::new("blur", vec![PortSpec::new("input", PortKind::Mask)], PortKind::Mask)
                .with_pad_grow(24)
                .boxed(),
        )
        .add_node("out", passthrough(PortKind::Mask, PortKind::Mask))
        .connect("src", "blur", "input")
        .connect("blur", "out", "input")
        .set_output("out");
    let g = b.build().unwrap();
    let pads = g.compute_pad(8).unwrap();
    // out: 8 (doc pad). blur upstream of out, so it sees 8 then declares
    // 32 upstream. src therefore needs 32.
    let p = |id: &str| {
        let ix = g.topo_order().iter().find(|&&i| g.node_id(i) == id).unwrap();
        pads[*ix]
    };
    assert_eq!(p("out"), 8);
    assert_eq!(p("blur"), 8);
    assert_eq!(p("src"), 32);
}

// -- M1: registry + build_graph round-trips ---------------------------------

struct SrcFactory(PortKind);
impl NodeFactory for SrcFactory {
    fn build(
        &self,
        _fields: &serde_json::Map<String, serde_json::Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        Ok(BuiltNode {
            node: Mock::new("src", vec![], self.0).boxed(),
            connections: vec![],
        })
    }
}

struct BlurFactory;
impl NodeFactory for BlurFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, serde_json::Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = crate::take_input_ref(fields, "input")?;
        let sigma = fields
            .get("sigma")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| FactoryError::MissingField("sigma".into()))?;
        let pad = (sigma * 3.0).ceil() as u32;
        Ok(BuiltNode {
            node: Mock::new(
                "blur",
                vec![PortSpec::new("input", PortKind::Mask)],
                PortKind::Mask,
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
    r.register("image", SrcFactory(PortKind::Mask));
    r.register("blur", BlurFactory);
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
    // image returns Mask, but if we somehow connected a Raster source...
    // We'll register a Raster-emitting "rsrc" and try to feed blur with it.
    let mut reg = test_registry();
    reg.register("rsrc", SrcFactory(PortKind::Raster));
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

// -- M2: evaluator + cache --------------------------------------------------

/// Node that counts how often it has been eval'd. Useful for cache tests.
struct Counter {
    op: &'static str,
    output: PortKind,
    inputs: Vec<PortSpec>,
    count: std::sync::atomic::AtomicU32,
    salt: u32,
}

impl Counter {
    fn new(op: &'static str, output: PortKind) -> Arc<Self> {
        Arc::new(Self {
            op,
            output,
            inputs: vec![],
            count: 0.into(),
            salt: 0,
        })
    }
    fn count(&self) -> u32 {
        self.count.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Node for Counter {
    fn op_name(&self) -> &'static str {
        self.op
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.inputs
    }
    fn output(&self) -> PortKind {
        self.output
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        _inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        self.count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let size = ctx.canvas.padded_size();
        Ok(PortValue::Mask(Arc::new(MaskBuf::filled(
            size, size, 0.25,
        ))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(self.op.as_bytes());
        h.update(&self.salt.to_le_bytes());
    }
}

/// Wrap an `Arc<dyn Node>` so it can be inserted into the builder
/// without losing its identity (we need to read the count back later).
struct Forward(Arc<dyn Node>);
impl Node for Forward {
    fn op_name(&self) -> &'static str {
        self.0.op_name()
    }
    fn inputs(&self) -> &[PortSpec] {
        self.0.inputs()
    }
    fn output(&self) -> PortKind {
        self.0.output()
    }
    fn coord_space(&self) -> CoordSpace {
        self.0.coord_space()
    }
    fn required_pad(&self, d: u32) -> u32 {
        self.0.required_pad(d)
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        self.0.eval(ctx, inputs)
    }
    fn param_hash(&self, h: &mut Xxh3) {
        self.0.param_hash(h)
    }
}

fn small_canvas() -> CanvasInfo {
    CanvasInfo {
        tile_size: 8,
        pad: 0,
    }
}

#[test]
fn evaluator_returns_output_value() {
    let mut b = GraphBuilder::new();
    b.add_node("a", Box::new(Forward(Counter::new("src", PortKind::Mask))))
        .set_output("a");
    let g = b.build().unwrap();
    let cache = Cache::new();
    let assets = NoAssets;
    let ev = Evaluator::new(&g, &cache, &assets);
    let out = ev
        .render(
            TileId { z: 0, x: 0, y: 0 },
            small_canvas(),
            &ParamValues::new(),
            0,
        )
        .unwrap();
    let mask = out.as_mask().unwrap();
    assert_eq!(mask.width, 8);
    assert!((mask.pixel(0, 0) - 0.25).abs() < 1e-6);
}

#[test]
fn evaluator_evaluates_each_node_once_per_render() {
    // diamond: a -> b, a -> c, b+c -> d. `a` should eval ONCE.
    let counter = Counter::new("src", PortKind::Mask);
    let pass = |op: &'static str| {
        Mock::new(op, vec![PortSpec::new("input", PortKind::Mask)], PortKind::Mask).boxed()
    };
    let merge = Mock::new(
        "merge",
        vec![
            PortSpec::new("left", PortKind::Mask),
            PortSpec::new("right", PortKind::Mask),
        ],
        PortKind::Mask,
    )
    .boxed();

    let mut b = GraphBuilder::new();
    b.add_node(
        "a",
        Box::new(Forward(Arc::clone(&counter) as Arc<dyn Node>)),
    )
    .add_node("b", pass("b"))
    .add_node("c", pass("c"))
    .add_node("d", merge)
    .connect("a", "b", "input")
    .connect("a", "c", "input")
    .connect("b", "d", "left")
    .connect("c", "d", "right")
    .set_output("d");
    let g = b.build().unwrap();
    let cache = Cache::new();
    let assets = NoAssets;
    let ev = Evaluator::new(&g, &cache, &assets);
    ev.render(
        TileId { z: 0, x: 0, y: 0 },
        small_canvas(),
        &ParamValues::new(),
        0,
    )
    .unwrap();
    assert_eq!(counter.count(), 1, "shared upstream should eval once");
}

#[test]
fn cache_reuses_results_across_renders() {
    let counter = Counter::new("src", PortKind::Mask);
    let mut b = GraphBuilder::new();
    b.add_node(
        "a",
        Box::new(Forward(Arc::clone(&counter) as Arc<dyn Node>)),
    )
    .set_output("a");
    let g = b.build().unwrap();
    let cache = Cache::new();
    let assets = NoAssets;
    let ev = Evaluator::new(&g, &cache, &assets);
    let tile = TileId { z: 0, x: 0, y: 0 };
    let cv = small_canvas();
    ev.render(tile, cv, &ParamValues::new(), 0).unwrap();
    ev.render(tile, cv, &ParamValues::new(), 0).unwrap();
    assert_eq!(counter.count(), 1, "second render should hit cache");
    // Different tile -> miss again.
    ev.render(TileId { z: 0, x: 1, y: 0 }, cv, &ParamValues::new(), 0)
        .unwrap();
    assert_eq!(counter.count(), 2);
}

#[test]
fn pad_exceeded_errors() {
    let mut b = GraphBuilder::new();
    b.add_node("src", src(PortKind::Mask))
        .add_node(
            "blur",
            Mock::new("blur", vec![PortSpec::new("input", PortKind::Mask)], PortKind::Mask)
                .with_pad_grow(crate::MAX_PAD + 1)
                .boxed(),
        )
        .connect("src", "blur", "input")
        .set_output("blur");
    let g = b.build().unwrap();
    match g.compute_pad(0) {
        Err(BuildError::PadExceeded { .. }) => {}
        other => panic!("expected PadExceeded, got {other:?}"),
    }
}
