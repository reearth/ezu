//! `switch` — pass one of its upstream nodes through.
//!
//! Two ways to say which:
//!
//! - **By condition**, `cases` with `default`: each case is
//!   `{ "when": <bool>, "then": "@node" }`, and the first whose `when`
//!   holds is taken, else `default`. MapLibre's `case`. A `when` is a
//!   literal, a `$param`, or an `@node` — an `expr` node, say.
//! - **By name**, `branches` with `select`: `branches` maps names to
//!   nodes, and `select` names one — a literal, or a `string` `$param`.
//!   MapLibre's `match`. `default` is taken for a name no branch has.
//!
//! A third, two-way form is deprecated: `a` / `b` with `select` — `"a"` /
//! `"b"`, or a `$param` / `@node` giving one (true picks `b`). It is a
//! one-case `cases` (or, for a `string` param, `branches` named `a` and
//! `b`) under other names, so it still builds, with a warning that spells
//! out that replacement.
//!
//! The choice is made when the graph is built if nothing in it can change
//! per render — a literal `select`, or `when`s that are literal up to the
//! first true one. Then the switch mirrors the chosen input's kind and
//! disappears from the type chain: one branch may be a raster and another
//! a set of features. Otherwise the choice is made per render, and every
//! branch must be the same kind, which the build checks rather than
//! surprising the downstream node.
//!
//! Either way only the chosen branch is evaluated. A choice the evaluator
//! can work out before it evaluates anything — a literal or a `$param` —
//! leaves every other branch, and whatever only it needs, unrendered
//! (see `Node::reads_input`). A `@node` condition is only known once it
//! has been evaluated, so from the first such `when` on, every branch it
//! could lead to is rendered.
//!
//! Use cases:
//! - Toggle a layer from the outside: a case `when: "$labels"` over two
//!   raster branches, flipped by a CLI flag, a query string, or a slider.
//! - Pick one of several themes by a `string` param with an `enum`.
//! - A/B compare two style variants from the same document, by editing
//!   one literal.

use std::sync::{Mutex, OnceLock};

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use ezu_style as spec;
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

/// How the branch is chosen.
enum Choice {
    /// Fixed when the graph is built: the branch's port.
    Static(usize),
    /// Two-way, per render: `false` picks `a` (port 0), `true` picks `b`.
    Bool(In<bool>),
    /// By name, per render, from a `string` param.
    Name {
        param: String,
        /// The param's declared default, used when nothing overrides it.
        fallback: String,
    },
    /// By condition, per render: one `when` per case, in order; a port
    /// past the last case is `default`.
    Cases(Vec<In<bool>>),
}

struct SwitchNode {
    choice: Choice,
    /// The `branches` names, in port order, for [`Choice::Name`].
    names: Vec<String>,
    /// Port of `default`, when there is one.
    default: Option<usize>,
    /// Ports `0..branches` are the branches (`default` among them); any
    /// after are `when` ports.
    branches: usize,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl SwitchNode {
    /// The port a name selects: its branch, else `default`.
    fn by_name(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name).or(self.default)
    }

    /// The value of a `string` param this render, or its default.
    fn param_name(ctx: &EvalCtx<'_>, param: &str, fallback: &str) -> Result<String, EvalError> {
        match ctx.params.get(param) {
            None => Ok(fallback.to_string()),
            Some(v) => v.as_str().map(str::to_string).ok_or_else(|| {
                EvalError::Other(format!(
                    "param `${param}`: expected string, got {}",
                    v.kind_name()
                ))
            }),
        }
    }

    /// The branch to pass through this render.
    fn index(&self, ctx: &EvalCtx<'_>, inputs: &[Option<PortValue>]) -> Result<usize, EvalError> {
        match &self.choice {
            Choice::Static(ix) => Ok(*ix),
            // `true` picks `b`, matching the literal bool form.
            Choice::Bool(sel) => Ok(usize::from(sel.get(ctx, inputs)?)),
            Choice::Name { param, fallback } => {
                let name = Self::param_name(ctx, param, fallback)?;
                self.by_name(&name).ok_or_else(|| {
                    EvalError::Other(format!(
                        "switch: `${param}` is `{name}`, which names no branch, and there is \
                         no `default`"
                    ))
                })
            }
            Choice::Cases(whens) => {
                for (ix, when) in whens.iter().enumerate() {
                    if when.get(ctx, inputs)? {
                        return Ok(ix);
                    }
                }
                Ok(self.default.expect("`cases` always has a default"))
            }
        }
    }

    /// Which branches this render may pass through, decided from what is
    /// known before any input is evaluated. A `@node` choice is unknown
    /// until then, so every branch it could lead to counts.
    fn live_branches(&self, ctx: &EvalCtx<'_>) -> Vec<bool> {
        let mut live = vec![false; self.branches];
        // A `$param` or a literal resolves without inputs; a port does not.
        let known = |c: &In<bool>| match c {
            In::Port { .. } => None,
            c => c.get(ctx, &[]).ok(),
        };
        match &self.choice {
            Choice::Static(ix) => live[*ix] = true,
            Choice::Bool(sel) => match known(sel) {
                Some(b) => live[usize::from(b)] = true,
                None => live.fill(true),
            },
            Choice::Name { param, fallback } => {
                // A name with no branch fails the render, which then has
                // nothing to evaluate.
                if let Some(ix) = Self::param_name(ctx, param, fallback)
                    .ok()
                    .and_then(|n| self.by_name(&n))
                {
                    live[ix] = true;
                }
            }
            Choice::Cases(whens) => {
                for (ix, when) in whens.iter().enumerate() {
                    match known(when) {
                        Some(true) => {
                            live[ix] = true;
                            return live;
                        }
                        Some(false) => {}
                        None => live[ix] = true,
                    }
                }
                if let Some(d) = self.default {
                    live[d] = true;
                }
            }
        }
        live
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
        if matches!(self.choice, Choice::Static(_)) {
            return Ok(());
        }
        let kinds: Vec<Option<PortKind>> = input_kinds[..self.branches].to_vec();
        let Some(Some(first)) = kinds.first().copied() else {
            return Err("every branch must be connected".to_string());
        };
        for (ix, kind) in kinds.iter().enumerate() {
            match kind {
                Some(k) if *k == first => {}
                Some(k) => {
                    return Err(format!(
                        "the branch is chosen per render, so every branch must be the same \
                         kind — `{}` is {first} and `{}` is {k}. Choose with literals to \
                         switch between different kinds, since that is fixed when the graph \
                         is built",
                        self.ports[0].name, self.ports[ix].name
                    ))
                }
                None => return Err("every branch must be connected".to_string()),
            }
        }
        Ok(())
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        // Mirror the chosen input's kind. A per-render choice has every
        // branch the same kind (see `validate_kinds`), so the first answers.
        let ix = match self.choice {
            Choice::Static(ix) => ix,
            _ => 0,
        };
        input_kinds[ix].unwrap_or(PortKind::Raster)
    }
    fn reads_input(&self, port: usize, ctx: &EvalCtx<'_>) -> bool {
        port >= self.branches || self.live_branches(ctx)[port]
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let ix = self.index(ctx, inputs)?;
        inputs[ix]
            .clone()
            .ok_or_else(|| EvalError::MissingInput(self.ports[ix].name.into()))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"switch");
        match &self.choice {
            Choice::Static(ix) => {
                h.update(&[0]);
                h.update(&(*ix as u64).to_le_bytes());
            }
            Choice::Bool(sel) => {
                h.update(&[1]);
                sel.param_hash(h);
            }
            Choice::Name { param, fallback } => {
                h.update(&[2]);
                h.update(param.as_bytes());
                h.update(&[0]);
                h.update(fallback.as_bytes());
            }
            Choice::Cases(whens) => {
                h.update(&[3]);
                for w in whens {
                    w.param_hash(h);
                }
            }
        }
        h.update(&(self.branches as u64).to_le_bytes());
        for n in &self.names {
            h.update(n.as_bytes());
            h.update(&[0]);
        }
        h.update(&[self.default.is_some() as u8]);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

/// Interned `&'static str` port names (`cases[0]`, `cases[0].when`, …).
/// `PortSpec::name` is `&'static str`; each pool grows once per distinct
/// index ever built and is shared by every `switch`, so the leak is
/// bounded by the widest switch seen, not by the number of builds.
fn port_name(
    pool: &'static OnceLock<Mutex<Vec<&'static str>>>,
    ix: usize,
    make: impl Fn(usize) -> String,
) -> &'static str {
    let mut pool = pool
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    while pool.len() <= ix {
        let name: &'static str = Box::leak(make(pool.len()).into_boxed_str());
        pool.push(name);
    }
    pool[ix]
}

fn case_then(ix: usize) -> &'static str {
    static POOL: OnceLock<Mutex<Vec<&'static str>>> = OnceLock::new();
    port_name(&POOL, ix, |i| format!("cases[{i}].then"))
}

fn case_when(ix: usize) -> &'static str {
    static POOL: OnceLock<Mutex<Vec<&'static str>>> = OnceLock::new();
    port_name(&POOL, ix, |i| format!("cases[{i}].when"))
}

fn branch(ix: usize) -> &'static str {
    static POOL: OnceLock<Mutex<Vec<&'static str>>> = OnceLock::new();
    port_name(&POOL, ix, |i| format!("branches[{i}]"))
}

/// Read a field that names a node, as `then` and the `branches` do.
fn node_ref(v: &Value, field: &str) -> Result<String, FactoryError> {
    let bad = || FactoryError::BadField {
        field: field.to_string(),
        msg: "expected a `@node` reference".into(),
    };
    match v.as_str().map(spec::FieldRef::classify) {
        Some(spec::FieldRef::Node(id)) => Ok(id.to_string()),
        _ => Err(bad()),
    }
}

fn any_port(name: &'static str) -> PortSpec {
    PortSpec {
        name,
        accepts: ACCEPTS_ANY,
        optional: false,
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
        let form = |name: &str| fields.contains_key(name);
        let conflict = |a: &str, b: &str| {
            Err(FactoryError::Custom(format!(
                "`{a}` and `{b}` are different forms of `switch`; give one"
            )))
        };
        if form("cases") {
            for other in ["branches", "a", "b", "select"] {
                if form(other) {
                    return conflict("cases", other);
                }
            }
            build_cases(fields, ctx)
        } else if form("branches") {
            for other in ["a", "b"] {
                if form(other) {
                    return conflict("branches", other);
                }
            }
            build_branches(fields, ctx)
        } else {
            if form("default") {
                return Err(FactoryError::BadField {
                    field: "default".into(),
                    msg: "`default` belongs to `cases` or `branches`; with `a` and `b`, \
                          `select` picks between the two"
                        .into(),
                });
            }
            build_two_way(fields, ctx)
        }
    }
    fn schema(&self) -> Value {
        let node = schema_frag::node_ref();
        serde_json::json!({
            "description": "Pass one of its inputs through. By condition, as MapLibre's `case`: the first of `cases` whose `when` holds, else `default`. By name, as MapLibre's `match`: `branches` maps names to nodes and `select` names one, with `default` for a name no branch has. The two-way form, `a` / `b` picked by `select`, is deprecated: it still works, and the build warns with the `cases` (or, for a `string` param, `branches`) form that does the same. Inputs accept any port kind. A choice fixed by literals is made when the graph is built, and the output mirrors the chosen input's kind — so branches may be different kinds. A `$param` or `@node` choice is made per render instead, which requires every branch to be the same kind. Only the chosen branch is rendered; a `@node` `when` is known only once evaluated, so every branch from the first one on is rendered with it.",
            "properties": {
                "a": {
                    "type": "string",
                    "pattern": "^@?[A-Za-z_][A-Za-z0-9_-]*$",
                    "deprecated": true,
                    "description": "Deprecated: use `cases`, with this as `default`. The branch taken when `select` is false or absent.",
                },
                "b": {
                    "type": "string",
                    "pattern": "^@?[A-Za-z_][A-Za-z0-9_-]*$",
                    "deprecated": true,
                    "description": "Deprecated: use `cases`, with this as the `then` of a case whose `when` is `select`. The branch taken when `select` is true.",
                },
                "select": schema_frag::in_string(serde_json::json!({
                    "description": "With `branches`: the name of the branch to take, or a `string` `$param` naming it, read per render. With the deprecated `a` / `b`: `\"a\"` (the default) or `\"b\"`, a bool `$param` or `@node` port (true picks `b`), or a `string` `$param` naming `a` or `b` — write a bool choice as `cases`, and a `string` param as `branches` named `a` and `b`.",
                })),
                "branches": {
                    "type": "object",
                    "minProperties": 1,
                    "additionalProperties": node.clone(),
                    "description": "Name → node; `select` names the one to take. A `$param` `select` must be a `string` param, and each value its `enum` allows must name a branch unless there is a `default`.",
                },
                "cases": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "required": ["when", "then"],
                        "additionalProperties": false,
                        "properties": {
                            "when": {
                                "oneOf": [
                                    { "type": "boolean" },
                                    {
                                        "type": "string",
                                        "pattern": "^[$@][A-Za-z_][A-Za-z0-9_-]*$",
                                        "description": "`$param` (bool) or `@node` scalar port (an `expr` of `type: bool`, say).",
                                    },
                                ],
                            },
                            "then": node.clone(),
                        },
                    },
                    "description": "Tried in order; the first whose `when` holds is taken. Needs `default`.",
                },
                "default": node,
            },
            "oneOf": [
                { "required": ["a", "b"] },
                { "required": ["branches", "select"] },
                { "required": ["cases", "default"] },
            ],
        })
    }
}

/// `a` / `b` with `select`, the deprecated two-way form.
fn build_two_way(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
) -> Result<BuiltNode, FactoryError> {
    let a = take_input_ref(fields, "a")?;
    let b = take_input_ref(fields, "b")?;

    let mut r = InReader::new(fields, ctx, 2);
    let select = fields.get("select").and_then(Value::as_str);
    let choice = match select.map(spec::FieldRef::classify) {
        // A `string` param names `a` or `b`, as the literal does.
        Some(spec::FieldRef::Param(p))
            if ctx
                .params
                .get(p)
                .is_some_and(|d| d.kind == spec::ParamKind::String) =>
        {
            name_choice(ctx, p, &["a", "b"], false)?
        }
        // Any other reference is a bool, read per render.
        Some(spec::FieldRef::Param(_) | spec::FieldRef::Node(_)) => {
            Choice::Bool(r.bool_or("select", false)?)
        }
        _ => Choice::Static(literal_select(fields)?),
    };
    ctx.warn(two_way_replacement(&choice, select, &a, &b));
    let parts = r.finish();
    let mut param_refs = parts.param_refs;
    if let Choice::Name { param, .. } = &choice {
        param_refs.push(param.clone());
    }

    let mut ports = vec![any_port("a"), any_port("b")];
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
            choice,
            names: vec!["a".into(), "b".into()],
            default: None,
            branches: 2,
            ports,
            param_refs,
        }),
        connections,
    })
}

/// The deprecation warning for the two-way form, spelling out the form
/// that does the same with this node's own inputs, so it can be pasted in.
/// A bool choice is a one-case `cases` (true picks `b`, as it did); a
/// `string` param naming `a` or `b` is `branches` with those names; a
/// literal choice is fixed, so the switch can go and the chosen node be
/// used in its place, or stay as a `cases` toggle flipped by editing `when`.
fn two_way_replacement(choice: &Choice, select: Option<&str>, a: &str, b: &str) -> String {
    let fix = match choice {
        Choice::Bool(_) => format!(
            r#"write "cases": [{{ "when": "{}", "then": "@{b}" }}], "default": "@{a}" instead"#,
            select.unwrap_or_default()
        ),
        Choice::Name { param, .. } => format!(
            r#"write "branches": {{ "a": "@{a}", "b": "@{b}" }}, "select": "${param}" instead"#
        ),
        Choice::Static(ix) => {
            let (chosen, on) = if *ix == 0 { (a, false) } else { (b, true) };
            format!(
                r#"use `@{chosen}` in place of this switch, or write "cases": [{{ "when": {on}, "then": "@{b}" }}], "default": "@{a}" to keep a toggle"#
            )
        }
        Choice::Cases(_) => unreachable!("the two-way form never chooses by cases"),
    };
    format!("`a` / `b` are deprecated: {fix}")
}

/// A `string` param choosing among `names`, checked against them: its
/// default, and every value its `enum` allows, must name one — or, with
/// `has_default`, may fall through to `default`. Without an `enum` any
/// string can arrive, so that needs a `default` too.
fn name_choice(
    ctx: &FactoryCtx<'_>,
    p: &str,
    names: &[&str],
    has_default: bool,
) -> Result<Choice, FactoryError> {
    let decl = ctx
        .params
        .get(p)
        .ok_or_else(|| FactoryError::UnknownParam(p.to_string()))?;
    let bad = |msg: String| FactoryError::BadField {
        field: "select".into(),
        msg,
    };
    if decl.kind != spec::ParamKind::String {
        return Err(bad(format!(
            "param `${p}` is declared `{}`, but naming a branch needs a `string` param",
            decl.kind.name()
        )));
    }
    let fallback = decl.default.as_str().unwrap_or_default().to_string();
    if !has_default {
        let known = |n: &str| names.contains(&n);
        let Some(choices) = &decl.choices else {
            return Err(bad(format!(
                "param `${p}` declares no `enum`, so it may name no branch at all: give it \
                 an `enum` of branch names ({}), or give the switch a `default`",
                names.join(", ")
            )));
        };
        let missing: Vec<String> = choices
            .iter()
            .chain(std::iter::once(&fallback))
            .filter(|c| !known(c))
            .map(|c| format!("`{c}`"))
            .collect();
        if !missing.is_empty() {
            return Err(bad(format!(
                "param `${p}` may be {}, which names no branch (branches: {}); add the \
                 branch, or a `default`",
                missing.join(", "),
                names.join(", ")
            )));
        }
    }
    Ok(Choice::Name {
        param: p.to_string(),
        fallback,
    })
}

/// `branches` with `select` and an optional `default`.
fn build_branches(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
) -> Result<BuiltNode, FactoryError> {
    let map = fields
        .get("branches")
        .and_then(Value::as_object)
        .filter(|m| !m.is_empty())
        .ok_or_else(|| FactoryError::BadField {
            field: "branches".into(),
            msg: "expected an object of names to `@node` references, with at least one".into(),
        })?;
    let mut names = Vec::with_capacity(map.len());
    let mut ports = Vec::with_capacity(map.len() + 1);
    let mut connections = Vec::with_capacity(map.len() + 1);
    for (ix, (name, v)) in map.iter().enumerate() {
        let src = node_ref(v, &format!("branches.{name}"))?;
        let port = branch(ix);
        names.push(name.clone());
        ports.push(any_port(port));
        connections.push(Connection {
            port: port.into(),
            src,
        });
    }
    let default = match fields.get("default") {
        None => None,
        Some(v) => {
            let src = node_ref(v, "default")?;
            ports.push(any_port("default"));
            connections.push(Connection {
                port: "default".into(),
                src,
            });
            Some(ports.len() - 1)
        }
    };

    let select = fields
        .get("select")
        .ok_or_else(|| FactoryError::MissingField("select".into()))?;
    let name_list: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut param_refs = Vec::new();
    let choice = match select.as_str().map(spec::FieldRef::classify) {
        Some(spec::FieldRef::Param(p)) => {
            param_refs.push(p.to_string());
            name_choice(ctx, p, &name_list, default.is_some())?
        }
        Some(spec::FieldRef::Literal(s)) => {
            let ix = names
                .iter()
                .position(|n| n == s)
                .or(default)
                .ok_or_else(|| FactoryError::BadField {
                    field: "select".into(),
                    msg: format!(
                        "`{s}` names no branch (branches: {}), and there is no `default`",
                        name_list.join(", ")
                    ),
                })?;
            Choice::Static(ix)
        }
        _ => {
            return Err(FactoryError::BadField {
                field: "select".into(),
                msg: "with `branches`, expected a branch name or a `string` `$param` — no node \
                      produces a string"
                    .into(),
            })
        }
    };
    let branches = ports.len();
    Ok(BuiltNode {
        node: Box::new(SwitchNode {
            choice,
            names,
            default,
            branches,
            ports,
            param_refs,
        }),
        connections,
    })
}

/// `cases` with `default`.
fn build_cases(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
) -> Result<BuiltNode, FactoryError> {
    let list = fields
        .get("cases")
        .and_then(Value::as_array)
        .filter(|l| !l.is_empty())
        .ok_or_else(|| FactoryError::BadField {
            field: "cases".into(),
            msg: "expected an array of `{ \"when\": …, \"then\": \"@node\" }`, with at least one"
                .into(),
        })?;
    let default_src = node_ref(
        fields
            .get("default")
            .ok_or_else(|| FactoryError::MissingField("default".into()))?,
        "default",
    )?;

    // Branch ports first — every `then`, then `default` — and the `when`
    // ports after them.
    let n = list.len();
    let mut r = InReader::new(fields, ctx, n + 1);
    let mut ports = Vec::with_capacity(n + 1);
    let mut connections = Vec::with_capacity(n + 1);
    let mut whens = Vec::with_capacity(n);
    for (ix, case) in list.iter().enumerate() {
        let obj = case.as_object().ok_or_else(|| FactoryError::BadField {
            field: format!("cases[{ix}]"),
            msg: "expected `{ \"when\": …, \"then\": \"@node\" }`".into(),
        })?;
        if let Some(extra) = obj.keys().find(|k| *k != "when" && *k != "then") {
            return Err(FactoryError::BadField {
                field: format!("cases[{ix}]"),
                msg: format!("unknown field `{extra}`; a case has `when` and `then`"),
            });
        }
        let then = obj
            .get("then")
            .ok_or_else(|| FactoryError::MissingField(format!("cases[{ix}].then")))?;
        let src = node_ref(then, &format!("cases[{ix}].then"))?;
        let port = case_then(ix);
        ports.push(any_port(port));
        connections.push(Connection {
            port: port.into(),
            src,
        });
        let when = obj
            .get("when")
            .ok_or_else(|| FactoryError::MissingField(format!("cases[{ix}].when")))?;
        whens.push(r.element::<bool>(case_when(ix), when)?);
    }
    ports.push(any_port("default"));
    connections.push(Connection {
        port: "default".into(),
        src: default_src,
    });
    let parts = r.finish();
    ports.extend(parts.ports);
    connections.extend(parts.connections);

    // Literal conditions up to the first true one decide it here.
    let mut choice = Choice::Static(n);
    for (ix, when) in whens.iter().enumerate() {
        match when {
            In::Const(false) => continue,
            In::Const(true) => choice = Choice::Static(ix),
            _ => choice = Choice::Cases(whens.clone()),
        }
        break;
    }

    Ok(BuiltNode {
        node: Box::new(SwitchNode {
            choice,
            names: Vec::new(),
            default: Some(n),
            branches: n + 1,
            ports,
            param_refs: parts.param_refs,
        }),
        connections,
    })
}

/// Resolve a literal `select` — `"a"` or `"b"`, `"a"` when absent — to
/// its port.
fn literal_select(fields: &serde_json::Map<String, Value>) -> Result<usize, FactoryError> {
    match read_optional_string(fields, "select")?.as_deref() {
        None | Some("a") => Ok(0),
        Some("b") => Ok(1),
        Some(other) => Err(FactoryError::BadField {
            field: "select".into(),
            msg: format!("expected `a`/`b`, or a `$param` / `@node` ref, got `{other}`"),
        }),
    }
}

ezu_graph::submit_node!(SwitchFactory);
