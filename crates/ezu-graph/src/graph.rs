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

    #[error(
        "type mismatch on `{node}.{port}`: expected {expected}, source `{src}` produces {got}"
    )]
    TypeMismatch {
        node: NodeId,
        port: String,
        src: NodeId,
        expected: PortKind,
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

    #[error(
        "required pad ({required}) on node `{node}` exceeds limit ({limit})"
    )]
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
    /// Output node index.
    output: NodeIx,
    /// Topological order, output last.
    topo: Vec<NodeIx>,
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

        for edge in &self.edges {
            let src_ix = ix_of(&edge.src).ok_or_else(|| BuildError::UnknownRef {
                from: edge.src.clone(),
                to: edge.dst.clone(),
            })?;
            let dst_ix = ix_of(&edge.dst).ok_or_else(|| BuildError::UnknownRef {
                from: edge.src.clone(),
                to: edge.dst.clone(),
            })?;

            let (_, dst_node) = self.nodes.get_index(dst_ix).unwrap();
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

            let src_kind = self.nodes.get_index(src_ix).unwrap().1.output();
            let expected = dst_node.inputs()[port_ix].kind;
            if src_kind != expected {
                return Err(BuildError::TypeMismatch {
                    node: edge.dst.clone(),
                    port: edge.dst_port.clone(),
                    src: edge.src.clone(),
                    expected,
                    got: src_kind,
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
        // Document output must be a canvas-padded raster — anything
        // smaller (e.g. a raw `Sprite`) will alias badly through the
        // host's `raster_to_png` crop. Catch this at build time.
        let output_kind = self.nodes.get_index(output_ix).unwrap().1.output();
        if output_kind != PortKind::Raster {
            return Err(BuildError::OutputKindMismatch {
                node: output_id.clone(),
                got: output_kind,
            });
        }

        let topo = topo_sort(n, &incoming, &self.nodes)?;

        Ok(Graph {
            nodes: self.nodes,
            incoming,
            outgoing,
            output: output_ix,
            topo,
        })
    }
}

impl Default for GraphBuilder {
    fn default() -> Self {
        Self::new()
    }
}

fn topo_sort(
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
        let bad = (0..n).find(|&i| indegree[i] != 0).unwrap();
        let (id, _) = nodes.get_index(bad).unwrap();
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
            .field("topo", &self.topo.iter().map(|&i| self.node_id(i)).collect::<Vec<_>>())
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

    pub fn node(&self, ix: NodeIx) -> &dyn Node {
        self.nodes.get_index(ix).unwrap().1.as_ref()
    }

    pub fn node_id(&self, ix: NodeIx) -> &str {
        self.nodes.get_index(ix).unwrap().0
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

    /// The source feeding `node.inputs()[port_ix]`, if connected.
    pub fn incoming(&self, ix: NodeIx, port_ix: usize) -> Option<NodeIx> {
        self.incoming[ix][port_ix]
    }

    /// Group nodes into evaluation "levels". A node's level is one more
    /// than the maximum level of its inputs (sources are at level 0).
    /// All nodes in the same level have no edges between them and can
    /// be evaluated in parallel. Returned as `levels[node_ix] = depth`.
    pub fn compute_levels(&self) -> Vec<u32> {
        let mut levels = vec![0u32; self.len()];
        for &ix in &self.topo {
            let max_up = self
                .upstream(ix)
                .map(|s| levels[s] + 1)
                .max()
                .unwrap_or(0);
            levels[ix] = max_up;
        }
        levels
    }

    /// Bucket nodes by level, preserving topo order within each bucket
    /// for determinism.
    pub fn level_buckets(&self) -> Vec<Vec<NodeIx>> {
        let levels = self.compute_levels();
        let max_level = levels.iter().copied().max().unwrap_or(0);
        let mut buckets: Vec<Vec<NodeIx>> = vec![Vec::new(); (max_level + 1) as usize];
        for &ix in &self.topo {
            buckets[levels[ix] as usize].push(ix);
        }
        buckets
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
