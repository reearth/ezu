//! Shared test helpers: mock nodes used across topology / build / eval
//! suites.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use xxhash_rust::xxh3::Xxh3;

use crate::{
    CanvasInfo, CoordSpace, EvalCtx, EvalError, Node, PortKind, PortSpec, PortValue, RasterBuf,
};

/// A mock node with configurable ports and pad growth.
pub(super) struct Mock {
    op: &'static str,
    inputs: Vec<PortSpec>,
    output: PortKind,
    pad_grow: u32,
    space: CoordSpace,
}

impl Mock {
    pub(super) fn new(op: &'static str, inputs: Vec<PortSpec>, output: PortKind) -> Self {
        Self {
            op,
            inputs,
            output,
            pad_grow: 0,
            space: CoordSpace::Inherit,
        }
    }
    pub(super) fn with_pad_grow(mut self, g: u32) -> Self {
        self.pad_grow = g;
        self
    }
    pub(super) fn boxed(self) -> Box<dyn Node> {
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
            other => panic!("mock node has no default output for {other:?}"),
        })
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(self.op.as_bytes());
        h.update(&self.pad_grow.to_le_bytes());
    }
}

pub(super) fn src(kind: PortKind) -> Box<dyn Node> {
    Mock::new("src", vec![], kind).boxed()
}

pub(super) fn passthrough(input: PortKind, output: PortKind) -> Box<dyn Node> {
    Mock::new("pass", vec![PortSpec::new("input", input)], output).boxed()
}

pub(super) fn small_canvas() -> CanvasInfo {
    CanvasInfo {
        tile_size: 8,
        pad: 0,
    }
}

/// Node that counts how often it has been eval'd. Used by cache tests.
pub(super) struct Counter {
    op: &'static str,
    output: PortKind,
    inputs: Vec<PortSpec>,
    count: AtomicU32,
    salt: u32,
}

impl Counter {
    pub(super) fn new(op: &'static str, output: PortKind) -> Arc<Self> {
        Arc::new(Self {
            op,
            output,
            inputs: vec![],
            count: 0.into(),
            salt: 0,
        })
    }
    pub(super) fn count(&self) -> u32 {
        self.count.load(Ordering::SeqCst)
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
        self.count.fetch_add(1, Ordering::SeqCst);
        let size = ctx.canvas.padded_size();
        Ok(PortValue::Raster(Arc::new(RasterBuf::filled(
            size, size, [64, 0, 0, 64],
        ))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(self.op.as_bytes());
        h.update(&self.salt.to_le_bytes());
    }
}

/// Wraps an `Arc<dyn Node>` so it can be inserted into the builder
/// without losing identity — useful when tests need to read state
/// (like `Counter::count`) back after the graph owns the node.
pub(super) struct Forward(pub Arc<dyn Node>);

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
