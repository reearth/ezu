//! Node registry — maps op names to factory functions.
//!
//! The style parser (`ezu-style`) produces a [`spec::Document`] whose
//! nodes carry an `op: String` and an opaque map of fields. The registry
//! turns those entries into typed [`Node`] instances plus the list of
//! input ports to wire up.
//!
//! Node implementations live in `ezu-paint` (and any downstream crate);
//! they register themselves with a [`NodeRegistry`] which the application
//! hands to [`build_graph`](crate::build_graph).

use std::collections::HashMap;

use ezu_style as spec;

use crate::node::Node;

/// One input port that a node wants connected, recorded by name.
#[derive(Debug, Clone)]
pub struct Connection {
    /// Name of the input port on the node being built.
    pub port: String,
    /// Referenced node id (without the `@` prefix).
    pub src: String,
}

/// What a [`NodeFactory`] returns: the constructed node plus its
/// requested input wiring. The graph builder applies the connections
/// after every node has been constructed.
pub struct BuiltNode {
    pub node: Box<dyn Node>,
    pub connections: Vec<Connection>,
}

/// Read-only context handed to factories: lets them resolve `$param`
/// and asset references during construction.
pub struct FactoryCtx<'a> {
    pub params: &'a indexmap::IndexMap<String, spec::ParamDecl>,
    pub assets: &'a indexmap::IndexMap<String, spec::AssetDecl>,
}

#[derive(Debug, thiserror::Error)]
pub enum FactoryError {
    #[error("missing required field `{0}`")]
    MissingField(String),
    #[error("field `{field}` has wrong type: {msg}")]
    BadField { field: String, msg: String },
    #[error("unknown param reference `${0}`")]
    UnknownParam(String),
    #[error("unknown asset reference `@{0}`")]
    UnknownAsset(String),
    #[error("{0}")]
    Custom(String),
}

/// Trait every op implementation provides one of.
///
/// Factories are typically zero-sized structs. They inspect the JSON
/// `fields` map, validate types, and return a [`BuiltNode`]. They MUST
/// NOT execute any rendering — only construction.
pub trait NodeFactory: Send + Sync {
    fn build(
        &self,
        fields: &serde_json::Map<String, serde_json::Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError>;
}

/// Catalog of registered ops, keyed by op name.
#[derive(Default)]
pub struct NodeRegistry {
    ops: HashMap<&'static str, Box<dyn NodeFactory>>,
}

impl NodeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, op_name: &'static str, factory: impl NodeFactory + 'static) {
        self.ops.insert(op_name, Box::new(factory));
    }

    pub fn get(&self, op_name: &str) -> Option<&dyn NodeFactory> {
        self.ops.get(op_name).map(|b| b.as_ref())
    }
}

/// Helper for factory authors: extract a `@node-ref` from a string field.
///
/// Returns the bare node id (no `@`). Errors if the field is missing,
/// not a string, or not a node reference.
pub fn take_input_ref(
    fields: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<String, FactoryError> {
    let v = fields
        .get(name)
        .ok_or_else(|| FactoryError::MissingField(name.to_string()))?;
    let s = v.as_str().ok_or_else(|| FactoryError::BadField {
        field: name.to_string(),
        msg: "expected string node reference".into(),
    })?;
    match spec::FieldRef::classify(s) {
        spec::FieldRef::Node(id) => Ok(id.to_string()),
        _ => Err(FactoryError::BadField {
            field: name.to_string(),
            msg: format!("expected `@node-ref`, got `{s}`"),
        }),
    }
}
