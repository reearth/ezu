//! The `Node` trait — what every operation in the graph implements.

use xxhash_rust::xxh3::Xxh3;

use crate::eval::{EvalCtx, EvalError};
use crate::port::{CoordSpace, PortKind, PortSpec};
use crate::value::PortValue;

/// One operation in the DAG. Stored as `Box<dyn Node>` inside
/// [`crate::Graph`]; the graph never mutates a node after construction.
pub trait Node: Send + Sync {
    /// Stable identifier for the operation (e.g. `"blur"`,
    /// `"scatter-dabs"`). Matches the `op` field in the style JSON.
    fn op_name(&self) -> &'static str;

    /// Declared input ports in positional order. The style JSON
    /// connects each port by name; `eval` receives values in this same
    /// positional order.
    fn inputs(&self) -> &[PortSpec];

    /// The kind of value this node produces.
    ///
    /// `input_kinds` carries the resolved [`PortKind`] of each input
    /// port, in the same positional order as [`Node::inputs`]. Entries
    /// are `Some` for connected ports (including optional ones) and
    /// `None` for unconnected optional ports.
    ///
    /// Most nodes return a constant; polymorphic nodes (e.g. `blur`
    /// accepting both `Raster` and `Sprite`) inspect `input_kinds` and
    /// mirror the upstream kind. The graph builder resolves nodes in
    /// topological order, so upstream kinds are always known when this
    /// is called.
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind;

    /// Reject a combination of upstream kinds this node cannot serve,
    /// with a message explaining why.
    ///
    /// [`Node::output`] has to answer with *some* kind, so a node whose
    /// requirement spans several ports — `switch` with a runtime
    /// `select`, which can only promise one output kind if both of its
    /// inputs share one — says so here instead. Called once per node at
    /// build time, right after the upstream kinds are resolved and
    /// before `output`.
    fn validate_kinds(&self, _input_kinds: &[Option<PortKind>]) -> Result<(), String> {
        Ok(())
    }

    /// Coordinate space the node operates in. Defaults to inheriting
    /// from inputs.
    fn coord_space(&self) -> CoordSpace {
        CoordSpace::Inherit
    }

    /// How much canvas padding this node requires *upstream* given the
    /// padding requested by downstream consumers. Blur-like ops grow
    /// the value; most pass it through unchanged.
    fn required_pad(&self, downstream: u32) -> u32 {
        downstream
    }

    /// Produce this node's output given resolved inputs. `inputs` has
    /// one entry per declared port, in the order returned by
    /// [`Node::inputs`]; unconnected optional ports are `None`.
    fn eval(&self, ctx: &EvalCtx<'_>, inputs: &[Option<PortValue>])
        -> Result<PortValue, EvalError>;

    /// Stable content hash of this node's *own* parameters (not inputs).
    /// Used as part of the cache key. Implementations should feed every
    /// configuration value that influences output into the hasher.
    fn param_hash(&self, hasher: &mut Xxh3);

    /// Named asset bindings this node samples via the
    /// [`AssetLoader`](crate::eval::AssetLoader). The evaluator folds
    /// each binding's `AssetLoader::hash` into this node's cache key,
    /// so changes in bound data invalidate caches automatically. Like
    /// declaring uniforms in a shader.
    ///
    /// Default: no bindings.
    fn asset_inputs(&self) -> Vec<String> {
        Vec::new()
    }

    /// Named document params this node reads from
    /// [`EvalCtx::params`](crate::eval::EvalCtx) at eval time (fields
    /// built from `$param` references). The evaluator folds each
    /// referenced param's *runtime* value into this node's cache key,
    /// so overriding a param invalidates exactly the nodes that read
    /// it — and nothing else.
    ///
    /// Default: no param reads.
    fn param_refs(&self) -> Vec<String> {
        Vec::new()
    }
}
