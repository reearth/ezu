//! Drive [`GraphBuilder`] from a parsed [`spec::Document`] using a
//! [`NodeRegistry`].

use ezu_style as spec;

use crate::graph::{BuildError, Graph, GraphBuilder};
use crate::port::PortKind;
use crate::registry::{FactoryCtx, FactoryError, NodeRegistry};

#[derive(Debug, thiserror::Error)]
pub enum BuildGraphError {
    #[error("unknown op `{op}` on node `{node}`")]
    UnknownOp { node: String, op: String },

    #[error("factory error on node `{node}`: {source}")]
    Factory {
        node: String,
        #[source]
        source: FactoryError,
    },

    #[error(transparent)]
    Expand(#[from] spec::ExpandError),

    #[error(
        "call `{call}` of `{func}`: input `{input}` expects {expected}, but `@{src}` produces {got}"
    )]
    FuncInputKind {
        call: String,
        func: String,
        input: String,
        expected: PortKind,
        src: String,
        got: PortKind,
    },

    #[error("call `{call}` of `{func}`: declared output-kind is {declared}, but the body produces {got}")]
    FuncOutputKind {
        call: String,
        func: String,
        declared: PortKind,
        got: PortKind,
    },

    #[error("legend entry `{label}` names `@{src}`, which is not a node in this style")]
    LegendUnknownNode { label: String, src: String },

    #[error("legend entry `{label}` names `@{src}`, which produces {got} — a legend entry must name a node that draws something ({expected})")]
    LegendNodeKind {
        label: String,
        src: String,
        expected: PortKind,
        got: PortKind,
    },

    #[error(transparent)]
    Graph(#[from] BuildError),
}

fn port_kind(k: spec::FuncKind) -> PortKind {
    match k {
        spec::FuncKind::Features => PortKind::Features,
        spec::FuncKind::Raster => PortKind::Raster,
        spec::FuncKind::Sprite => PortKind::Sprite,
        spec::FuncKind::Brush => PortKind::Brush,
        spec::FuncKind::Scalar => PortKind::Scalar,
        spec::FuncKind::ScalarField => PortKind::ScalarField,
    }
}

/// Build a typed [`Graph`] from a parsed document and a registry of
/// node factories. Documents with a `functions` block are expanded
/// inline first; declared input/output kinds are verified against the
/// built graph's resolved port kinds.
pub fn build_graph(
    doc: &spec::Document,
    registry: &NodeRegistry,
) -> Result<Graph, BuildGraphError> {
    let expanded = spec::expand_functions(doc)?;
    let (doc, kind_checks) = match &expanded {
        Some(e) => (&e.doc, e.kind_checks.as_slice()),
        None => (doc, &[][..]),
    };

    let ctx = FactoryCtx {
        params: &doc.params,
        sources: &doc.sources,
    };

    let mut gb = GraphBuilder::new();
    let mut pending: Vec<(String, Vec<crate::registry::Connection>)> = Vec::new();

    for (id, spec) in &doc.nodes {
        let factory = registry
            .get(&spec.op)
            .ok_or_else(|| BuildGraphError::UnknownOp {
                node: id.clone(),
                op: spec.op.clone(),
            })?;

        let built = factory
            .build(&spec.fields, &ctx)
            .map_err(|e| BuildGraphError::Factory {
                node: id.clone(),
                source: e,
            })?;

        gb.add_node(id.clone(), built.node);
        pending.push((id.clone(), built.connections));
    }

    for (dst, conns) in pending {
        for c in conns {
            gb.connect(c.src, dst.clone(), c.port);
        }
    }

    gb.set_output(doc.output.as_str().to_string());
    let graph = gb.build()?;

    // Verify each call site's declared kinds against the resolved port
    // kinds. Argument sources and the call's output node are plain
    // graph nodes after expansion, so this is a pure lookup.
    for check in kind_checks {
        let Some(ix) = graph.index_of(&check.node) else {
            // The referenced node failed to resolve — the builder has
            // already reported the real error path; skip.
            continue;
        };
        let got = graph.output_kind(ix);
        let expected = port_kind(check.declared);
        if got != expected {
            return Err(match &check.input {
                Some(input) => BuildGraphError::FuncInputKind {
                    call: check.call.clone(),
                    func: check.func.clone(),
                    input: input.clone(),
                    expected,
                    src: check.node.clone(),
                    got,
                },
                None => BuildGraphError::FuncOutputKind {
                    call: check.call.clone(),
                    func: check.func.clone(),
                    declared: expected,
                    got,
                },
            });
        }
    }

    // The legend is not part of the graph, but its entries point into
    // it: each one names the node that draws the symbol it explains. A
    // dangling or non-drawing reference is a broken legend, and a broken
    // legend is worse than none — check it here, where every caller
    // already passes.
    if let Some(legend) = &doc.legend {
        for entry in &legend.entries {
            let src = entry.from.as_str();
            let Some(ix) = graph.index_of(src) else {
                return Err(BuildGraphError::LegendUnknownNode {
                    label: entry.label.clone(),
                    src: src.to_string(),
                });
            };
            let got = graph.output_kind(ix);
            if got != PortKind::Raster {
                return Err(BuildGraphError::LegendNodeKind {
                    label: entry.label.clone(),
                    src: src.to_string(),
                    expected: PortKind::Raster,
                    got,
                });
            }
        }
    }
    Ok(graph)
}
