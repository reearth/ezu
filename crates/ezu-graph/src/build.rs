//! Drive [`GraphBuilder`] from a parsed [`spec::Document`] using a
//! [`NodeRegistry`].

use ezu_style as spec;

use crate::graph::{BuildError, Graph, GraphBuilder};
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
    Graph(#[from] BuildError),
}

/// Build a typed [`Graph`] from a parsed document and a registry of
/// node factories.
pub fn build_graph(
    doc: &spec::Document,
    registry: &NodeRegistry,
) -> Result<Graph, BuildGraphError> {
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
    Ok(gb.build()?)
}
