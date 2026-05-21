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
    /// Op name used as the registry key (e.g. `"solid"`, `"fill-solid"`).
    fn op_name(&self) -> &'static str;

    fn build(
        &self,
        fields: &serde_json::Map<String, serde_json::Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError>;

    /// JSON Schema fragment describing this op's field shape. Should
    /// return a JSON object with `properties` and (optionally)
    /// `required` keys — the `op` const is added by the registry.
    ///
    /// The default is permissive (any fields allowed). Override to opt
    /// in to editor autocomplete and client-side validation.
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({})
    }
}

/// A factory submitted statically via [`inventory::submit!`]. Built-in
/// node crates use this to self-register without touching a central
/// list — see [`NodeRegistry::from_inventory`].
pub struct StaticOp(pub &'static dyn NodeFactory);

inventory::collect!(StaticOp);

/// Submit a unit-struct [`NodeFactory`] to the global inventory so
/// [`NodeRegistry::from_inventory`] picks it up.
///
/// ```ignore
/// pub(super) struct SolidFactory;
/// impl NodeFactory for SolidFactory { /* ... */ }
/// ezu_graph::submit_node!(SolidFactory);
/// ```
#[macro_export]
macro_rules! submit_node {
    ($factory:ident) => {
        $crate::inventory::submit! {
            $crate::StaticOp(&$factory)
        }
    };
}

/// Catalog of registered ops, keyed by op name.
#[derive(Default)]
pub struct NodeRegistry {
    ops: HashMap<&'static str, &'static dyn NodeFactory>,
}

impl NodeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry from every [`StaticOp`] submitted via
    /// [`inventory::submit!`] across the linked binary.
    pub fn from_inventory() -> Self {
        let mut r = Self::default();
        for StaticOp(f) in inventory::iter::<StaticOp> {
            r.register_static(*f);
        }
        r
    }

    /// Register a factory by leaking it into `'static`. Convenient for
    /// dynamic registration; built-in ops should prefer
    /// [`inventory::submit!`] + [`Self::from_inventory`].
    pub fn register(&mut self, factory: impl NodeFactory + 'static) {
        self.register_static(Box::leak(Box::new(factory)));
    }

    /// Register a `'static` factory reference (the form produced by
    /// [`inventory::submit!`]).
    pub fn register_static(&mut self, factory: &'static dyn NodeFactory) {
        self.ops.insert(factory.op_name(), factory);
    }

    pub fn get(&self, op_name: &str) -> Option<&dyn NodeFactory> {
        self.ops.get(op_name).copied()
    }

    /// All registered op names, sorted for deterministic output.
    pub fn op_names(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self.ops.keys().copied().collect();
        names.sort_unstable();
        names
    }

    /// Build a JSON Schema for a complete style document by gathering
    /// each registered op's [`NodeFactory::schema`] under a `oneOf`. The
    /// returned value is suitable for serving at
    /// `/schemas/ezu-style.json` and feeding to editor tooling
    /// (Monaco, vscode-json-languageservice, ajv, …).
    pub fn document_schema(&self) -> serde_json::Value {
        use serde_json::{json, Value};
        let mut variants: Vec<Value> = Vec::with_capacity(self.ops.len());
        for op in self.op_names() {
            let factory = self.ops.get(op).unwrap();
            let mut schema = factory.schema();
            if !schema.is_object() {
                schema = json!({});
            }
            let obj = schema.as_object_mut().unwrap();
            obj.entry("type")
                .or_insert_with(|| json!("object"));
            // Add the discriminator field.
            let props = obj
                .entry("properties")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .unwrap();
            props.insert(
                "op".to_string(),
                json!({ "const": op, "description": format!("Selects the `{op}` operation.") }),
            );
            // Require `op`, preserving any other required fields.
            let required = obj
                .entry("required")
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .unwrap();
            if !required.iter().any(|v| v.as_str() == Some("op")) {
                required.insert(0, json!("op"));
            }
            obj.insert("title".to_string(), json!(format!("op: {op}")));
            variants.push(schema);
        }

        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Ezu Style Spec",
            "type": "object",
            "required": ["name", "nodes", "output"],
            "properties": {
                "name": { "type": "string" },
                "version": { "type": "string" },
                "tile-size": { "type": "integer", "minimum": 1 },
                "pad": { "type": "integer", "minimum": 0 },
                "params": {
                    "type": "object",
                    "additionalProperties": {
                        "type": "object",
                        "required": ["type", "default"],
                        "properties": {
                            "type": { "enum": ["color", "number", "bool"] },
                            "default": {},
                            "min": { "type": "number" },
                            "max": { "type": "number" },
                            "description": { "type": "string" }
                        }
                    }
                },
                "assets": {
                    "type": "object",
                    "additionalProperties": {
                        "type": "object",
                        "required": ["type", "src"],
                        "properties": {
                            "type": { "enum": ["brush", "image", "mask-image", "gradient"] },
                            "src": { "type": "string" }
                        }
                    }
                },
                "nodes": {
                    "type": "object",
                    "additionalProperties": { "oneOf": variants }
                },
                "output": {
                    "type": "string",
                    "description": "Node id of the final raster (with or without `@`)."
                }
            }
        })
    }
}

/// Pre-built JSON Schema fragments commonly reused by `NodeFactory::schema`
/// implementations.
pub mod schema_frag {
    use serde_json::{json, Value};

    /// A reference to another node, written `@id`.
    pub fn node_ref() -> Value {
        json!({
            "type": "string",
            "pattern": "^@?[A-Za-z_][A-Za-z0-9_-]*$",
            "description": "Reference to another node (`@name`)."
        })
    }

    /// A reference to a registered asset (brush / image / etc.).
    pub fn asset_ref() -> Value {
        json!({
            "type": "string",
            "description": "Asset reference (`@name`) or literal path."
        })
    }

    /// `#rrggbb` or `#rrggbbaa` color literal. Also allows `$param`.
    pub fn color() -> Value {
        json!({
            "type": "string",
            "pattern": "^(#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?|\\$[A-Za-z_][A-Za-z0-9_-]*)$",
            "description": "sRGB hex color, or `$param` reference."
        })
    }

    /// Number in `[0, 1]` — commonly opacity / fraction parameters.
    pub fn unit_number() -> Value {
        json!({ "type": "number", "minimum": 0.0, "maximum": 1.0 })
    }

    /// Non-negative number in pixels.
    pub fn px_number() -> Value {
        json!({ "type": "number", "minimum": 0.0 })
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

/// Like [`take_input_ref`] but returns `None` if the field is absent
/// or JSON `null`. Use for optional input ports.
pub fn take_optional_input_ref(
    fields: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<Option<String>, FactoryError> {
    match fields.get(name) {
        None => Ok(None),
        Some(v) if v.is_null() => Ok(None),
        Some(_) => Ok(Some(take_input_ref(fields, name)?)),
    }
}
