//! The `Node` trait — what every operation in the graph implements.

use xxhash_rust::xxh3::Xxh3;

use crate::eval::{AssetLoader, EvalCtx, EvalError};
use crate::port::{CoordSpace, PortKind, PortSpec};
use crate::value::PortValue;

/// How far a brush puts ink from the path it is dragged along.
///
/// Carries the radius it was measured at, because an op may override the
/// brush's radius — everything else about a dab (its elliptical ratio,
/// its jitters) scales with the radius, so the reach does too.
#[derive(Debug, Clone, Copy)]
pub struct InkReach {
    /// Furthest a dab reaches from the path, at `radius_px`.
    pub reach_px: f64,
    /// The radius that reach was measured at.
    pub radius_px: f64,
}

impl InkReach {
    /// The reach this brush would have at a different radius.
    pub fn at_radius(&self, radius_px: f64) -> f64 {
        if self.radius_px <= 0.0 {
            return self.reach_px;
        }
        self.reach_px * (radius_px / self.radius_px).max(1.0)
    }
}

/// What a node needs in order to say how far outside the canvas its
/// input geometry can still matter — see [`Node::influence_pad`].
pub struct InfluenceCtx<'a> {
    /// Reach already claimed by everything downstream of this node.
    pub downstream: u32,
    /// The brush feeding this node, if one does — see
    /// [`Node::ink_reach`].
    pub brush: Option<InkReach>,
    pub assets: &'a dyn AssetLoader,
}

impl InfluenceCtx<'_> {
    /// The reach is not bounded by anything this node knows, so nothing
    /// upstream of it may be dropped.
    pub const UNBOUNDED: u32 = u32::MAX;

    /// `downstream` plus `extra` px, saturating into
    /// [`Self::UNBOUNDED`] rather than wrapping.
    pub fn plus(&self, extra: f64) -> u32 {
        if !extra.is_finite() || extra < 0.0 {
            return Self::UNBOUNDED;
        }
        let extra = extra.ceil();
        if extra >= u32::MAX as f64 {
            return Self::UNBOUNDED;
        }
        self.downstream.saturating_add(extra as u32)
    }

    /// `downstream` plus a bound that may not exist — an `@node` port
    /// or an unbounded `$param` has no static ceiling, and a field that
    /// moves geometry outward without one cannot be culled against.
    pub fn plus_bound(&self, bound: Option<f64>) -> u32 {
        match bound {
            Some(b) => self.plus(b.abs()),
            None => Self::UNBOUNDED,
        }
    }
}

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

    /// How far outside the canvas this node's *input geometry* can still
    /// end up mattering, given the distance already claimed downstream.
    ///
    /// This is the mirror of [`Node::required_pad`], and deliberately a
    /// separate number. `required_pad` asks how much canvas a node needs
    /// because it *reads* neighbouring pixels; a brush stroke reads
    /// nothing, so it declares none and the canvas stays small. But a
    /// stroke *writes* a dab's radius away from its vertex, and a wave
    /// displaces a vertex by its amplitude before that — so geometry
    /// sitting outside the canvas can still put ink inside it. Answering
    /// "how far outside?" is what lets a source drop geometry it can
    /// prove is invisible, which is the difference between a deeply
    /// overzoomed tile costing its ancestor's whole extent and costing
    /// its own.
    ///
    /// Raster ops inherit their read distance, since ink pulled inward by
    /// a blur matters as much as ink painted there. Ops that displace or
    /// grow geometry add their own reach. Return [`u32::MAX`] to say the
    /// reach cannot be bounded, which keeps every upstream feature.
    ///
    /// Like `required_pad`, this must be a worst case over the values a
    /// field can take, not the value this render happens to use.
    fn influence_pad(&self, ctx: &InfluenceCtx<'_>) -> u32 {
        self.required_pad(ctx.downstream)
    }

    /// How far from a stroke's path this node's *output* can lay ink,
    /// for the op that consumes it. Only a brush answers: how wide a dab
    /// reaches is a property of the brush, not of the op holding it, and
    /// the op receives it through a port it cannot inspect until eval.
    /// The graph hands it to the consumer as [`InfluenceCtx::brush`].
    ///
    /// `None` from a node that *is* a brush means its reach could not be
    /// established, which leaves the consumer unbounded.
    fn ink_reach(&self, _assets: &dyn AssetLoader) -> Option<InkReach> {
        None
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
