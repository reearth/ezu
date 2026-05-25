//! `switch` — pick one of two upstream nodes by a build-time `select`
//! parameter. Both inputs may be of any port kind; the selected
//! input's resolved kind becomes this node's output kind, so the
//! switch effectively disappears from the type chain at build time.
//!
//! Use cases:
//! - A/B compare two style variants from the same document by
//!   flipping `select` (and a `params` binding, so an outer caller
//!   can drive it without editing the document).
//! - Toggle between a heavyweight and lightweight rendering branch
//!   based on a host-set parameter.

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::read_optional_string;

/// Every PortKind currently in the system. `switch`'s ports accept
/// all of them; the graph builder picks the actual kind from whichever
/// upstream is connected.
const ACCEPTS_ANY: &[PortKind] = &[
    PortKind::Features,
    PortKind::Raster,
    PortKind::Sprite,
    PortKind::Brush,
    PortKind::Scalar,
    PortKind::ScalarField,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Select {
    A,
    B,
}

struct SwitchNode {
    select: Select,
}

impl Node for SwitchNode {
    fn op_name(&self) -> &'static str {
        "switch"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[
            PortSpec {
                name: "a",
                accepts: ACCEPTS_ANY,
                optional: false,
            },
            PortSpec {
                name: "b",
                accepts: ACCEPTS_ANY,
                optional: false,
            },
        ];
        SPECS
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        // Mirror the selected input's kind. The other input is dead
        // weight at runtime but still type-checks independently.
        let idx = match self.select {
            Select::A => 0,
            Select::B => 1,
        };
        input_kinds[idx].unwrap_or(PortKind::Raster)
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let idx = match self.select {
            Select::A => 0,
            Select::B => 1,
        };
        inputs[idx]
            .clone()
            .ok_or_else(|| EvalError::MissingInput(if idx == 0 { "a" } else { "b" }.into()))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"switch");
        h.update(&[matches!(self.select, Select::B) as u8]);
    }
}

pub(super) struct SwitchFactory;
impl NodeFactory for SwitchFactory {
    fn op_name(&self) -> &'static str {
        "switch"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let a = take_input_ref(fields, "a")?;
        let b = take_input_ref(fields, "b")?;
        let select = match read_optional_string(fields, "select")?.as_deref() {
            None | Some("a") => Select::A,
            Some("b") => Select::B,
            Some(other) => {
                // Also accept booleans / integers so `$variant` params
                // can be wired in without forcing a string indirection.
                let raw = fields.get("select");
                if let Some(v) = raw {
                    if let Some(b_val) = v.as_bool() {
                        if b_val {
                            Select::B
                        } else {
                            Select::A
                        }
                    } else if let Some(n) = v.as_u64() {
                        if n == 0 {
                            Select::A
                        } else {
                            Select::B
                        }
                    } else {
                        return Err(FactoryError::BadField {
                            field: "select".into(),
                            msg: format!("expected `a`/`b` (or bool / 0/1), got `{other}`"),
                        });
                    }
                } else {
                    return Err(FactoryError::BadField {
                        field: "select".into(),
                        msg: format!("expected `a` or `b`, got `{other}`"),
                    });
                }
            }
        };
        let _ = ctx;
        Ok(BuiltNode {
            node: Box::new(SwitchNode { select }),
            connections: vec![
                Connection {
                    port: "a".into(),
                    src: a,
                },
                Connection {
                    port: "b".into(),
                    src: b,
                },
            ],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Pick `a` or `b` based on `select` (default `a`). Both inputs accept any port kind; the output's kind mirrors the selected input. Useful for A/B comparison and param-driven branching.",
            "properties": {
                "a": schema_frag::node_ref(),
                "b": schema_frag::node_ref(),
                "select": {
                    "oneOf": [
                        { "type": "string", "enum": ["a", "b"] },
                        { "type": "boolean" },
                        { "type": "integer", "minimum": 0, "maximum": 1 },
                    ],
                    "default": "a",
                },
            },
            "required": ["a", "b"],
        })
    }
}

ezu_graph::submit_node!(SwitchFactory);
