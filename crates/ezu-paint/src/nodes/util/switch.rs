//! `switch` — pick one of two upstream nodes by `select`.
//!
//! `select` decides at build time when it is a literal (`"a"` / `"b"`, a
//! bool, or `0`/`1`), and per render when it is a `$param` or an `@node`
//! scalar port.
//!
//! The difference is what the output's kind can be. With a literal the
//! chosen input is known before evaluation, so the switch mirrors that
//! input's kind and disappears from the type chain — `a` may be a raster
//! and `b` a set of features. With a runtime `select` the choice is not
//! known until the tile renders, so the node can only promise one kind:
//! both inputs must resolve to the same one, which it checks at build
//! time rather than surprising the downstream node.
//!
//! Use cases:
//! - Toggle a layer from the outside: `select: "$labels"` over two raster
//!   branches, flipped by a CLI flag, a query string, or a slider.
//! - A/B compare two style variants from the same document, by editing
//!   one literal.

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
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

/// How the branch is chosen: fixed when the graph is built, or read per
/// render from a param / scalar port.
enum SelectMode {
    Static(Select),
    Dynamic(In<bool>),
}

struct SwitchNode {
    select: SelectMode,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl SwitchNode {
    fn index(&self, ctx: &EvalCtx<'_>, inputs: &[Option<PortValue>]) -> Result<usize, EvalError> {
        Ok(match &self.select {
            SelectMode::Static(Select::A) => 0,
            SelectMode::Static(Select::B) => 1,
            // `true` picks `b`, matching the literal bool form.
            SelectMode::Dynamic(sel) => usize::from(sel.get(ctx, inputs)?),
        })
    }
}

impl Node for SwitchNode {
    fn op_name(&self) -> &'static str {
        "switch"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn validate_kinds(&self, input_kinds: &[Option<PortKind>]) -> Result<(), String> {
        if matches!(self.select, SelectMode::Static(_)) {
            return Ok(());
        }
        match (
            input_kinds.first().copied().flatten(),
            input_kinds.get(1).copied().flatten(),
        ) {
            (Some(a), Some(b)) if a == b => Ok(()),
            (Some(a), Some(b)) => Err(format!(
                "`select` is decided per render, so both inputs must be the same kind — \
                 `a` is {a} and `b` is {b}. Use a literal `select` to switch between \
                 different kinds, since that is fixed when the graph is built"
            )),
            _ => Err("`a` and `b` must both be connected".to_string()),
        }
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        // Mirror the selected input's kind. The other input is dead
        // weight at runtime but still type-checks independently. A
        // runtime `select` has both kinds equal (see `validate_kinds`),
        // so either answers.
        let idx = match self.select {
            SelectMode::Static(Select::B) => 1,
            _ => 0,
        };
        input_kinds[idx].unwrap_or(PortKind::Raster)
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let idx = self.index(ctx, inputs)?;
        inputs[idx]
            .clone()
            .ok_or_else(|| EvalError::MissingInput(if idx == 0 { "a" } else { "b" }.into()))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"switch");
        match &self.select {
            SelectMode::Static(s) => {
                h.update(&[0, matches!(s, Select::B) as u8]);
            }
            SelectMode::Dynamic(sel) => {
                h.update(&[1]);
                sel.param_hash(h);
            }
        }
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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

        // A `$param` / `@node` reference means the choice is made per
        // render; everything else is resolved here.
        let is_ref = fields
            .get("select")
            .and_then(Value::as_str)
            .is_some_and(|s| s.starts_with('$') || s.starts_with('@'));

        let mut r = InReader::new(fields, ctx, 2);
        let select = if is_ref {
            SelectMode::Dynamic(r.bool_or("select", false)?)
        } else {
            SelectMode::Static(literal_select(fields)?)
        };
        let parts = r.finish();

        let mut ports = vec![
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
        ports.extend(parts.ports);
        let mut connections = vec![
            Connection {
                port: "a".into(),
                src: a,
            },
            Connection {
                port: "b".into(),
                src: b,
            },
        ];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(SwitchNode {
                select,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Pick `a` or `b` based on `select` (default `a`). Both inputs accept any port kind. A literal `select` is resolved when the graph is built, and the output mirrors the chosen input's kind — so the two inputs may be different kinds. A `$param` or `@node` `select` is read per render instead, which requires both inputs to be the same kind.",
            "properties": {
                "a": schema_frag::node_ref(),
                "b": schema_frag::node_ref(),
                "select": {
                    "oneOf": [
                        { "type": "string", "enum": ["a", "b"] },
                        { "type": "boolean" },
                        { "type": "integer", "minimum": 0, "maximum": 1 },
                        {
                            "type": "string",
                            "pattern": "^[$@][A-Za-z_][A-Za-z0-9_-]*$",
                            "description": "`$param` reference or `@node` scalar port, read per render. `true` / non-zero picks `b`.",
                        },
                    ],
                    "default": "a",
                },
            },
            "required": ["a", "b"],
        })
    }
}

/// Resolve a literal `select` — `"a"` / `"b"`, a bool, or `0` / `1`.
fn literal_select(fields: &serde_json::Map<String, Value>) -> Result<Select, FactoryError> {
    let bad = |got: String| FactoryError::BadField {
        field: "select".into(),
        msg: format!("expected `a`/`b`, a bool, 0/1, or a `$param` / `@node` ref, got `{got}`"),
    };
    match read_optional_string(fields, "select")?.as_deref() {
        None | Some("a") => Ok(Select::A),
        Some("b") => Ok(Select::B),
        Some(other) => {
            let raw = fields.get("select").ok_or_else(|| bad(other.to_string()))?;
            if let Some(v) = raw.as_bool() {
                Ok(if v { Select::B } else { Select::A })
            } else if let Some(n) = raw.as_u64() {
                Ok(if n == 0 { Select::A } else { Select::B })
            } else {
                Err(bad(other.to_string()))
            }
        }
    }
}

ezu_graph::submit_node!(SwitchFactory);
