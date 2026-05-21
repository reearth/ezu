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
    fn output(&self) -> PortKind;

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
    fn eval(&self, ctx: &EvalCtx<'_>, inputs: &[Option<PortValue>]) -> Result<PortValue, EvalError>;

    /// Stable content hash of this node's *own* parameters (not inputs).
    /// Used as part of the cache key. Implementations should feed every
    /// configuration value that influences output into the hasher.
    fn param_hash(&self, hasher: &mut Xxh3);
}
