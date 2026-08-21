//! The DAG itself: building, type-checking, topology, pad propagation.

use std::collections::VecDeque;

use indexmap::IndexMap;

use crate::node::Node;
use crate::port::PortKind;

/// Identifier for a node in the [`Graph`]. Matches the key used in the
/// style JSON's `nodes` object (e.g. `"water_paint"`).
pub type NodeId = String;

/// Compact internal index assigned during graph build. Stable for the
/// lifetime of the graph; used by topo-sorted operations.
pub type NodeIx = usize;

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("unknown node reference `{from}` -> `{to}`")]
    UnknownRef { from: NodeId, to: NodeId },

    #[error("node `{node}` has no input port named `{port}`")]
    UnknownPort { node: NodeId, port: String },

    #[error("port `{node}.{port}` already connected")]
    DuplicateEdge { node: NodeId, port: String },

    #[error("node `{node}` cannot serve these input kinds: {msg}")]
    UnsupportedKinds { node: NodeId, msg: String },

    #[error(
        "type mismatch on `{node}.{port}`: expected one of [{}], source `{src}` produces {got}",
        accepts.iter().map(|k| k.to_string()).collect::<Vec<_>>().join(", ")
    )]
    TypeMismatch {
        node: NodeId,
        port: String,
        src: NodeId,
        accepts: Vec<PortKind>,
        got: PortKind,
    },

    #[error("required port `{node}.{port}` is not connected")]
    MissingInput { node: NodeId, port: String },

    #[error("cycle detected involving node `{0}`")]
    Cycle(NodeId),

    #[error("output node `{0}` is not in the graph")]
    UnknownOutput(NodeId),

    #[error("graph has no output node")]
    NoOutput,

    #[error(
        "output node `{node}` produces `{got}`, but the document output must produce `raster` (canvas-padded). Pipe a sprite through `place`, `tiling`, or `stamp` first."
    )]
    OutputKindMismatch { node: NodeId, got: PortKind },

    #[error("required pad ({required}) on node `{node}` exceeds limit ({limit})")]
    PadExceeded {
        node: NodeId,
        required: u32,
        limit: u32,
    },
}

/// An edge in the DAG: `src.output` flows into `dst.inputs()[port_ix]`.
#[derive(Debug, Clone, Copy)]
pub struct Edge {
    pub src: NodeIx,
    pub dst: NodeIx,
    pub dst_port: usize,
}

/// A built, type-checked DAG. Tile-independent; build once per style
/// and evaluate many times.
pub struct Graph {
    nodes: IndexMap<NodeId, Box<dyn Node>>,
    /// Per-node, per-input-port edge source (None if unconnected and
    /// the port was optional).
    incoming: Vec<Vec<Option<NodeIx>>>,
    /// Adjacency for downstream walks: outgoing[src] -> list of dst.
    outgoing: Vec<Vec<NodeIx>>,
    /// Deduplicated downstream adjacency: outgoing_unique[src] -> each
    /// distinct dst once, even when several of dst's ports read `src`.
    /// Drives the readiness scheduler's per-dependency decrements.
    outgoing_unique: Vec<Vec<NodeIx>>,
    /// Count of distinct upstream nodes feeding each node.
    indegree: Vec<usize>,
    /// Output node index.
    output: NodeIx,
    /// Topological order, output last.
    topo: Vec<NodeIx>,
    /// Resolved output [`PortKind`] for every node, indexed by [`NodeIx`].
    /// Polymorphic nodes (e.g. `blur` accepting both `Raster` and
    /// `Sprite`) have their actual output kind decided here at build
    /// time based on their connected inputs.
    output_kinds: Vec<PortKind>,
}

/// Maximum allowed pad propagated to any node, in pixels. Prevents
/// runaway blurs from demanding multi-tile buffers.
pub const MAX_PAD: u32 = 256;

/// Builder for constructing a [`Graph`] programmatically. The style
/// parser will drive this from JSON later; tests use it directly.
pub struct GraphBuilder {
    nodes: IndexMap<NodeId, Box<dyn Node>>,
    /// Pending edges, recorded by name; resolved at `build()` time.
    edges: Vec<EdgeSpec>,
    output: Option<NodeId>,
}

struct EdgeSpec {
    src: NodeId,
    dst: NodeId,
    dst_port: String,
}

impl GraphBuilder {
    pub fn new() -> Self {
        Self {
            nodes: IndexMap::new(),
            edges: Vec::new(),
            output: None,
        }
    }

    pub fn add_node(&mut self, id: impl Into<NodeId>, node: Box<dyn Node>) -> &mut Self {
        self.nodes.insert(id.into(), node);
        self
    }

    pub fn connect(
        &mut self,
        src: impl Into<NodeId>,
        dst: impl Into<NodeId>,
        dst_port: impl Into<String>,
    ) -> &mut Self {
        self.edges.push(EdgeSpec {
            src: src.into(),
            dst: dst.into(),
            dst_port: dst_port.into(),
        });
        self
    }

    pub fn set_output(&mut self, id: impl Into<NodeId>) -> &mut Self {
        self.output = Some(id.into());
        self
    }

    pub fn build(self) -> Result<Graph, BuildError> {
        let n = self.nodes.len();
        let mut incoming: Vec<Vec<Option<NodeIx>>> = self
            .nodes
            .values()
            .map(|node| vec![None; node.inputs().len()])
            .collect();
        let mut outgoing: Vec<Vec<NodeIx>> = vec![Vec::new(); n];

        let ix_of = |id: &str| -> Option<NodeIx> { self.nodes.get_index_of(id) };

        // Pass 1: wire edges (no type check yet — output kinds may be
        // polymorphic and only resolvable in topo order).
        for edge in &self.edges {
            let src_ix = ix_of(&edge.src).ok_or_else(|| BuildError::UnknownRef {
                from: edge.src.clone(),
                to: edge.dst.clone(),
            })?;
            let dst_ix = ix_of(&edge.dst).ok_or_else(|| BuildError::UnknownRef {
                from: edge.src.clone(),
                to: edge.dst.clone(),
            })?;

            let (_, dst_node) = self
                .nodes
                .get_index(dst_ix)
                .expect("dst_ix came from ix_of and is in range");
            let port_ix = dst_node
                .inputs()
                .iter()
                .position(|p| p.name == edge.dst_port)
                .ok_or_else(|| BuildError::UnknownPort {
                    node: edge.dst.clone(),
                    port: edge.dst_port.clone(),
                })?;

            if incoming[dst_ix][port_ix].is_some() {
                return Err(BuildError::DuplicateEdge {
                    node: edge.dst.clone(),
                    port: edge.dst_port.clone(),
                });
            }

            incoming[dst_ix][port_ix] = Some(src_ix);
            outgoing[src_ix].push(dst_ix);
        }

        // Required-port check.
        for (ix, (id, node)) in self.nodes.iter().enumerate() {
            for (port_ix, port) in node.inputs().iter().enumerate() {
                if !port.optional && incoming[ix][port_ix].is_none() {
                    return Err(BuildError::MissingInput {
                        node: id.clone(),
                        port: port.name.to_string(),
                    });
                }
            }
        }

        let output_id = self.output.ok_or(BuildError::NoOutput)?;
        let output_ix = ix_of(&output_id).ok_or(BuildError::UnknownOutput(output_id.clone()))?;

        let topo = topo_sort(n, &incoming, &self.nodes, output_ix)?;

        // Pass 2: walk topo order, resolve each node's output kind from
        // its (already-resolved) upstream kinds, and check the upstream
        // kind against each input port's `accepts` list.
        let mut output_kinds: Vec<PortKind> = vec![PortKind::Raster; n];
        for &ix in &topo {
            let (id, node) = self.nodes.get_index(ix).expect("ix from topo is in range");
            let specs = node.inputs();
            let mut input_kinds: Vec<Option<PortKind>> = Vec::with_capacity(specs.len());
            for (port_ix, spec) in specs.iter().enumerate() {
                match incoming[ix][port_ix] {
                    Some(src_ix) => {
                        let src_kind = output_kinds[src_ix];
                        if !spec.accepts_kind(src_kind) {
                            let (src_id, _) = self
                                .nodes
                                .get_index(src_ix)
                                .expect("src_ix from incoming is in range");
                            return Err(BuildError::TypeMismatch {
                                node: id.clone(),
                                port: spec.name.to_string(),
                                src: src_id.clone(),
                                accepts: spec.accepts.to_vec(),
                                got: src_kind,
                            });
                        }
                        input_kinds.push(Some(src_kind));
                    }
                    None => input_kinds.push(None),
                }
            }
            if let Err(msg) = node.validate_kinds(&input_kinds) {
                return Err(BuildError::UnsupportedKinds {
                    node: id.clone(),
                    msg,
                });
            }
            output_kinds[ix] = node.output(&input_kinds);
        }

        // Document output must be a canvas-padded raster — anything
        // smaller (e.g. a raw `Sprite`) will alias badly through the
        // host's `raster_to_png` crop. Catch this at build time.
        let output_kind = output_kinds[output_ix];
        if output_kind != PortKind::Raster {
            return Err(BuildError::OutputKindMismatch {
                node: output_id.clone(),
                got: output_kind,
            });
        }

        // Deduplicated views used by the readiness scheduler: each node
        // has one decrement per distinct upstream, and reaching zero
        // triggers exactly one spawn per distinct downstream.
        let mut outgoing_unique = outgoing.clone();
        for dsts in &mut outgoing_unique {
            dsts.sort_unstable();
            dsts.dedup();
        }
        let indegree: Vec<usize> = incoming
            .iter()
            .map(|ports| {
                let mut srcs: Vec<NodeIx> = ports.iter().filter_map(|p| *p).collect();
                srcs.sort_unstable();
                srcs.dedup();
                srcs.len()
            })
            .collect();

        Ok(Graph {
            nodes: self.nodes,
            incoming,
            outgoing,
            outgoing_unique,
            indegree,
            output: output_ix,
            topo,
            output_kinds,
        })
    }
}

impl Default for GraphBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Evaluation order for the graph: a valid topological order, chosen to
/// keep few intermediates alive at once.
///
/// Any topological order is correct, but they differ enormously in peak
/// memory. Breadth-first (plain Kahn) evaluates every source, then every
/// node above them, so a wide graph holds a full canvas-sized buffer for
/// each parallel branch before the node that consumes them ever runs.
/// Depth-first from the output instead finishes one input subtree before
/// starting its sibling, so a branch's buffers are released as soon as
/// their consumer has run.
///
/// Kahn still runs first, to detect cycles and to order any nodes the
/// output does not depend on (which are appended, after everything the
/// output needs).
fn topo_sort(
    n: usize,
    incoming: &[Vec<Option<NodeIx>>],
    nodes: &IndexMap<NodeId, Box<dyn Node>>,
    output: NodeIx,
) -> Result<Vec<NodeIx>, BuildError> {
    let kahn = kahn_order(n, incoming, nodes)?;

    // Depth-first post-order from the output: a node is emitted only
    // after every node it reads. Iterative, so a deep chain cannot blow
    // the stack.
    let mut order = Vec::with_capacity(n);
    let mut seen = vec![false; n];
    let mut stack: Vec<(NodeIx, usize)> = vec![(output, 0)];
    seen[output] = true;
    while let Some((ix, port)) = stack.pop() {
        // Ports are visited in declaration order, which for the
        // accumulator-style nodes (`stack`, `blend`) means the running
        // base is resolved before the layers composited onto it.
        match incoming[ix].get(port) {
            Some(&edge) => {
                stack.push((ix, port + 1));
                if let Some(src) = edge {
                    if !seen[src] {
                        seen[src] = true;
                        stack.push((src, 0));
                    }
                }
            }
            None => order.push(ix),
        }
    }
    // Nodes the output does not depend on: keep them, in Kahn order, so
    // a document is still free to evaluate a node for its own sake.
    order.extend(kahn.into_iter().filter(|&ix| !seen[ix]));
    Ok(order)
}

fn kahn_order(
    n: usize,
    incoming: &[Vec<Option<NodeIx>>],
    nodes: &IndexMap<NodeId, Box<dyn Node>>,
) -> Result<Vec<NodeIx>, BuildError> {
    // Kahn's algorithm over the unique upstream set per node.
    let mut indegree: Vec<usize> = incoming
        .iter()
        .map(|ports| {
            let mut srcs: Vec<NodeIx> = ports.iter().filter_map(|p| *p).collect();
            srcs.sort_unstable();
            srcs.dedup();
            srcs.len()
        })
        .collect();

    // Reverse adjacency: for each src, the unique dsts that depend on it.
    let mut rev: Vec<Vec<NodeIx>> = vec![Vec::new(); n];
    for (dst, ports) in incoming.iter().enumerate() {
        let mut srcs: Vec<NodeIx> = ports.iter().filter_map(|p| *p).collect();
        srcs.sort_unstable();
        srcs.dedup();
        for src in srcs {
            rev[src].push(dst);
        }
    }

    let mut queue: VecDeque<NodeIx> = (0..n).filter(|&i| indegree[i] == 0).collect();
    let mut order = Vec::with_capacity(n);
    while let Some(ix) = queue.pop_front() {
        order.push(ix);
        for &dst in &rev[ix] {
            indegree[dst] -= 1;
            if indegree[dst] == 0 {
                queue.push_back(dst);
            }
        }
    }

    if order.len() != n {
        // `order.len() != n` means at least one node still has incoming
        // edges (otherwise topo would have queued it). Find one such
        // node to name in the error.
        let bad = (0..n)
            .find(|&i| indegree[i] != 0)
            .expect("order.len() != n implies some indegree is non-zero");
        let (id, _) = nodes.get_index(bad).expect("bad < n is within nodes range");
        return Err(BuildError::Cycle(id.clone()));
    }
    Ok(order)
}

impl std::fmt::Debug for Graph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ids: Vec<&str> = self.nodes.keys().map(String::as_str).collect();
        f.debug_struct("Graph")
            .field("nodes", &ids)
            .field("output", &self.node_id(self.output))
            .field(
                "topo",
                &self
                    .topo
                    .iter()
                    .map(|&i| self.node_id(i))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Graph {
    /// Number of nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn output(&self) -> NodeIx {
        self.output
    }

    /// Topological order; output node is last.
    pub fn topo_order(&self) -> &[NodeIx] {
        &self.topo
    }

    /// The union of every node's [`Node::asset_inputs`], deduplicated and
    /// ordered. A host consults this to learn which named bindings the
    /// document's graph samples — including the `@<dx>,<dy>` neighbour
    /// names (see [`crate::neighbor`]) a cross-tile node requests, so it
    /// can fetch and bind exactly the neighbour tiles the graph needs
    /// rather than the whole 3×3 window unconditionally.
    pub fn asset_inputs(&self) -> std::collections::BTreeSet<String> {
        self.nodes.values().flat_map(|n| n.asset_inputs()).collect()
    }

    pub fn node(&self, ix: NodeIx) -> &dyn Node {
        self.nodes
            .get_index(ix)
            .expect("NodeIx is always within self.nodes range")
            .1
            .as_ref()
    }

    pub fn node_id(&self, ix: NodeIx) -> &str {
        self.nodes
            .get_index(ix)
            .expect("NodeIx is always within self.nodes range")
            .0
    }

    /// Look up a node's index by id.
    pub fn index_of(&self, id: &str) -> Option<NodeIx> {
        self.nodes.get_index_of(id)
    }

    /// Upstream nodes feeding `ix`, deduplicated.
    pub fn upstream(&self, ix: NodeIx) -> impl Iterator<Item = NodeIx> + '_ {
        let mut srcs: Vec<NodeIx> = self.incoming[ix].iter().filter_map(|p| *p).collect();
        srcs.sort_unstable();
        srcs.dedup();
        srcs.into_iter()
    }

    /// Downstream nodes consuming `ix`'s output (may contain duplicates
    /// if the same node connects multiple of its input ports to `ix`).
    pub fn downstream(&self, ix: NodeIx) -> &[NodeIx] {
        &self.outgoing[ix]
    }

    /// Distinct downstream nodes consuming `ix`'s output, each listed once.
    pub fn downstream_unique(&self, ix: NodeIx) -> &[NodeIx] {
        &self.outgoing_unique[ix]
    }

    /// Number of distinct upstream nodes feeding `ix` (0 for sources).
    pub fn indegree(&self, ix: NodeIx) -> usize {
        self.indegree[ix]
    }

    /// The source feeding `node.inputs()[port_ix]`, if connected.
    pub fn incoming(&self, ix: NodeIx, port_ix: usize) -> Option<NodeIx> {
        self.incoming[ix][port_ix]
    }

    /// Resolved output [`PortKind`] for `ix`. Decided at build time;
    /// polymorphic nodes' kind is fixed once the graph is built.
    pub fn output_kind(&self, ix: NodeIx) -> PortKind {
        self.output_kinds[ix]
    }

    /// Compute the canvas padding each node must supply, given the
    /// document-level `pad` requested at the output.
    pub fn compute_pad(&self, doc_pad: u32) -> Result<Vec<u32>, BuildError> {
        let mut required = vec![0u32; self.len()];
        required[self.output] = doc_pad;
        for &ix in self.topo.iter().rev() {
            let down = required[ix];
            let up = self.node(ix).required_pad(down);
            if up > MAX_PAD {
                return Err(BuildError::PadExceeded {
                    node: self.node_id(ix).to_string(),
                    required: up,
                    limit: MAX_PAD,
                });
            }
            for src in self.upstream(ix) {
                required[src] = required[src].max(up);
            }
        }
        Ok(required)
    }
}
