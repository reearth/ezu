//! `In<T>` — a scalar node field that accepts an inline literal, a
//! `$param` reference (resolved against [`EvalCtx::params`] at eval
//! time), or a `@node` connection on a [`PortKind::Scalar`] input port.
//!
//! Factories read fields through an [`InReader`], which classifies each
//! value and accumulates the scalar [`PortSpec`]s / [`Connection`]s /
//! param names the node must expose:
//!
//! ```ignore
//! let mut r = InReader::new(fields, ctx, FIXED_PORTS.len());
//! let sigma = r.number("sigma")?;          // In<f64>
//! let color = r.color_or("color", WHITE)?; // In<[f32; 4]>
//! let parts = r.finish();
//! // node stores `sigma`, `color`, `parts.ports` (appended after the
//! // fixed ports), returns `parts.param_refs` from `Node::param_refs`,
//! // and the factory returns `parts.connections` alongside any fixed
//! // ones.
//! ```
//!
//! A field that takes one of a fixed set of names reads through
//! [`InReader::enum_or`] into an [`InEnum`], which follows a `string`
//! `$param` the same way; [`read_enum_baked`] is its build-time
//! counterpart for fields that decide a node's shape.
//!
//! Cache correctness: `In::param_hash` feeds the *static* identity
//! (literal value / param name / port index) into `Node::param_hash`;
//! the runtime value of a `$param` is folded into the cache key by the
//! evaluator via [`Node::param_refs`](crate::node::Node::param_refs),
//! and a port-fed value arrives through the upstream node's hash.

use ezu_style as spec;
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::eval::{EvalCtx, EvalError};
use crate::port::{PortKind, PortSpec};
use crate::registry::{Connection, FactoryCtx, FactoryError};
use crate::value::{PortValue, ScalarValue};

/// Accepts-list for a scalar input port.
pub const ACCEPTS_SCALAR: &[PortKind] = &[PortKind::Scalar];

/// A scalar field type readable through [`In<T>`] / [`InReader`].
pub trait ScalarType: Copy {
    /// Human-readable type name for error messages.
    const NAME: &'static str;
    /// Whether a param declaration of `kind` supplies this type.
    fn matches_kind(kind: spec::ParamKind) -> bool;
    /// Extract from a runtime scalar value.
    fn from_scalar(v: &ScalarValue) -> Option<Self>;
    /// Parse from a JSON literal (field value or param default).
    fn from_json(v: &Value) -> Option<Self>;
    /// Feed into a cache-key hasher.
    fn hash_into(&self, h: &mut Xxh3);
    /// Clamp to a declaration's `min` / `max`. Identity for non-numbers.
    fn clamp_decl(self, _min: Option<f64>, _max: Option<f64>) -> Self {
        self
    }
}

impl ScalarType for f64 {
    const NAME: &'static str = "number";
    fn matches_kind(kind: spec::ParamKind) -> bool {
        kind == spec::ParamKind::Number
    }
    fn from_scalar(v: &ScalarValue) -> Option<Self> {
        v.as_number()
    }
    fn from_json(v: &Value) -> Option<Self> {
        v.as_f64()
    }
    fn hash_into(&self, h: &mut Xxh3) {
        h.update(&self.to_le_bytes());
    }
    fn clamp_decl(self, min: Option<f64>, max: Option<f64>) -> Self {
        let mut v = self;
        if let Some(m) = min {
            v = v.max(m);
        }
        if let Some(m) = max {
            v = v.min(m);
        }
        v
    }
}

/// Straight (non-premultiplied) sRGB-encoded RGBA in `[0, 1]` — the
/// same convention as a parsed `#rrggbb[aa]` literal.
impl ScalarType for [f32; 4] {
    const NAME: &'static str = "color";
    fn matches_kind(kind: spec::ParamKind) -> bool {
        kind == spec::ParamKind::Color
    }
    fn from_scalar(v: &ScalarValue) -> Option<Self> {
        v.as_color()
    }
    fn from_json(v: &Value) -> Option<Self> {
        spec::parse_hex_color(v.as_str()?)
    }
    fn hash_into(&self, h: &mut Xxh3) {
        for c in self {
            h.update(&c.to_le_bytes());
        }
    }
}

impl ScalarType for bool {
    const NAME: &'static str = "bool";
    fn matches_kind(kind: spec::ParamKind) -> bool {
        kind == spec::ParamKind::Bool
    }
    fn from_scalar(v: &ScalarValue) -> Option<Self> {
        v.as_bool()
    }
    fn from_json(v: &Value) -> Option<Self> {
        v.as_bool()
    }
    fn hash_into(&self, h: &mut Xxh3) {
        h.update(&[*self as u8]);
    }
}

/// A node field whose value is an inline literal, a `$param` reference,
/// or a scalar input port. Resolve with [`In::get`] inside `Node::eval`.
#[derive(Debug, Clone)]
pub enum In<T> {
    /// Inline literal, baked at build time.
    Const(T),
    /// `$param` reference: read `EvalCtx::params` at eval time, fall
    /// back to the declaration default, clamp numbers to the
    /// declaration's `min` / `max`.
    Param {
        name: String,
        fallback: T,
        min: Option<f64>,
        max: Option<f64>,
    },
    /// `@node` connection on a scalar input port.
    Port {
        /// Positional index into `Node::inputs()`.
        ix: usize,
        /// Field name, for error messages.
        name: &'static str,
    },
}

impl<T: ScalarType> In<T> {
    /// Resolve the field's value for one eval.
    pub fn get(&self, ctx: &EvalCtx<'_>, inputs: &[Option<PortValue>]) -> Result<T, EvalError> {
        match self {
            In::Const(v) => Ok(*v),
            In::Param {
                name,
                fallback,
                min,
                max,
            } => {
                let v = match ctx.params.get(name) {
                    None => *fallback,
                    Some(sv) => T::from_scalar(&sv).ok_or_else(|| {
                        EvalError::Other(format!(
                            "param `${name}`: expected {}, got {}",
                            T::NAME,
                            sv.kind_name()
                        ))
                    })?,
                };
                Ok(v.clamp_decl(*min, *max))
            }
            In::Port { ix, name } => {
                let v = inputs
                    .get(*ix)
                    .and_then(|o| o.as_ref())
                    .ok_or_else(|| EvalError::MissingInput((*name).into()))?;
                let PortValue::Scalar(sv) = v else {
                    return Err(EvalError::Other(format!(
                        "port `{name}`: expected a scalar, got {}",
                        v.kind()
                    )));
                };
                T::from_scalar(sv).ok_or_else(|| {
                    EvalError::Other(format!(
                        "port `{name}`: expected {}, got {}",
                        T::NAME,
                        sv.kind_name()
                    ))
                })
            }
        }
    }

    /// Feed the field's *static* identity into `Node::param_hash`.
    /// Runtime `$param` values are keyed via `Node::param_refs`; a
    /// port-fed value is keyed via the upstream node's hash.
    pub fn param_hash(&self, h: &mut Xxh3) {
        match self {
            In::Const(v) => {
                h.update(b"c");
                v.hash_into(h);
            }
            In::Param { name, fallback, .. } => {
                h.update(b"p");
                h.update(name.as_bytes());
                fallback.hash_into(h);
            }
            In::Port { ix, .. } => {
                h.update(b"@");
                h.update(&(*ix as u64).to_le_bytes());
            }
        }
    }

    /// Static upper bound usable for build-time decisions (pad
    /// propagation): the literal value, or a `$param`'s declared `max`.
    /// `None` for ports and unbounded params — pad-affecting fields
    /// must reject those at build time.
    pub fn static_bound(&self) -> Option<f64>
    where
        T: Into<f64>,
    {
        match self {
            In::Const(v) => Some((*v).into()),
            In::Param { max, .. } => *max,
            In::Port { .. } => None,
        }
    }
}

/// The names a field with a fixed set of values accepts, each paired with
/// the value it selects. Two names may select the same value (an alias);
/// the first spelling of a value is its canonical one.
pub type EnumTable<E> = &'static [(&'static str, E)];

/// A field that takes one of a fixed set of names — a blend mode, a line
/// cap, a noise type — written as a literal or as a `$param` of kind
/// `string`. Resolve with [`InEnum::get`] inside `Node::eval`, so a
/// render-time override of the param reaches the field.
///
/// There is no `@node` form: no node produces a string.
#[derive(Debug, Clone)]
pub struct InEnum<E: 'static> {
    table: EnumTable<E>,
    field: &'static str,
    src: EnumSrc<E>,
}

#[derive(Debug, Clone)]
enum EnumSrc<E> {
    Const(E),
    Param { name: String, fallback: E },
}

impl<E: Copy + PartialEq + 'static> InEnum<E> {
    /// A field fixed to `value` — for a node built in code rather than
    /// read from a style.
    pub fn constant(field: &'static str, table: EnumTable<E>, value: E) -> Self {
        Self {
            table,
            field,
            src: EnumSrc::Const(value),
        }
    }

    /// Resolve the field's value for one eval.
    pub fn get(&self, ctx: &EvalCtx<'_>) -> Result<E, EvalError> {
        match &self.src {
            EnumSrc::Const(v) => Ok(*v),
            EnumSrc::Param { name, fallback } => match ctx.params.get(name) {
                None => Ok(*fallback),
                Some(sv) => {
                    let Some(s) = sv.as_str() else {
                        return Err(EvalError::Other(format!(
                            "param `${name}`: expected string, got {}",
                            sv.kind_name()
                        )));
                    };
                    lookup(self.table, s).ok_or_else(|| {
                        EvalError::Other(format!(
                            "param `${name}`: `{s}` is not a value `{}` accepts (expected {})",
                            self.field,
                            names(self.table)
                        ))
                    })
                }
            },
        }
    }

    /// The value used when nothing overrides it: the literal, or the
    /// param's declared default. For build-time decisions that only need
    /// a representative value (a schema dump, a debug label).
    pub fn fallback(&self) -> E {
        match &self.src {
            EnumSrc::Const(v) | EnumSrc::Param { fallback: v, .. } => *v,
        }
    }

    /// The `$param` this field follows, if it follows one.
    pub fn param_name(&self) -> Option<&str> {
        match &self.src {
            EnumSrc::Const(_) => None,
            EnumSrc::Param { name, .. } => Some(name),
        }
    }

    /// Feed the field's *static* identity into `Node::param_hash`. The
    /// render-time value of a `$param` is keyed by the evaluator through
    /// `Node::param_refs`, as for [`In`].
    pub fn param_hash(&self, h: &mut Xxh3) {
        let ix = |v: E| self.table.iter().position(|(_, e)| *e == v).unwrap_or(0) as u64;
        match &self.src {
            EnumSrc::Const(v) => {
                h.update(b"c");
                h.update(&ix(*v).to_le_bytes());
            }
            EnumSrc::Param { name, fallback } => {
                h.update(b"p");
                h.update(name.as_bytes());
                h.update(&ix(*fallback).to_le_bytes());
            }
        }
    }
}

/// Every name a table accepts, aliases included, in table order — the
/// `enum` of the field's schema, so the schema cannot drift from what
/// the field parses.
pub fn enum_names<E>(table: EnumTable<E>) -> Vec<&'static str> {
    table.iter().map(|(n, _)| *n).collect()
}

fn lookup<E: Copy>(table: EnumTable<E>, s: &str) -> Option<E> {
    table.iter().find(|(n, _)| *n == s).map(|(_, e)| *e)
}

fn names<E>(table: EnumTable<E>) -> String {
    table.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
}

/// Check that a `$param` can stand in a field with a fixed set of names:
/// it must be a `string` param, its default must be one of the names,
/// and so must every value its `enum` allows. Without an `enum` any
/// string can arrive at render time; a name the field does not know then
/// fails that render, and declaring the `enum` moves the failure here.
fn check_enum_param<E: Copy>(
    field: &str,
    p: &str,
    decl: &spec::ParamDecl,
    table: EnumTable<E>,
) -> Result<E, FactoryError> {
    let bad = |msg: String| FactoryError::BadField {
        field: field.to_string(),
        msg,
    };
    if decl.kind != spec::ParamKind::String {
        return Err(bad(format!(
            "param `${p}` is declared `{}`, but this field takes one of {} — declare it \
             `\"type\": \"string\"`",
            decl.kind.name(),
            names(table)
        )));
    }
    if let Some(choices) = &decl.choices {
        let unknown: Vec<&str> = choices
            .iter()
            .map(String::as_str)
            .filter(|c| lookup(table, c).is_none())
            .collect();
        if !unknown.is_empty() {
            return Err(bad(format!(
                "param `${p}` allows {}, which this field does not accept (expected {})",
                unknown
                    .iter()
                    .map(|u| format!("`{u}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
                names(table)
            )));
        }
    }
    let default = decl.default.as_str().unwrap_or_default();
    lookup(table, default).ok_or_else(|| {
        bad(format!(
            "param `${p}` defaults to `{default}`, which this field does not accept \
             (expected {})",
            names(table)
        ))
    })
}

/// Read a literal name of a fixed-set field, or report what it accepts.
fn enum_literal<E: Copy>(field: &str, v: &Value, table: EnumTable<E>) -> Result<E, FactoryError> {
    let bad = |msg: String| FactoryError::BadField {
        field: field.to_string(),
        msg,
    };
    let Some(s) = v.as_str() else {
        return Err(bad(format!("expected one of {}", names(table))));
    };
    match spec::FieldRef::classify(s) {
        spec::FieldRef::Node(_) => Err(bad(format!(
            "expected one of {} or a `$param`, got a `@node` ref — no node produces a string",
            names(table)
        ))),
        _ => lookup(table, s).ok_or_else(|| {
            bad(format!(
                "unknown {field} `{s}`, expected one of {}",
                names(table)
            ))
        }),
    }
}

/// Read a fixed-set field the graph needs before it can evaluate
/// anything — one that decides a node's ports or output kind. A
/// `$param` is accepted and checked as [`InReader::enum_or`] checks it,
/// but only its declared default is used, and the build says so with a
/// warning, as for any field read at build time.
pub fn read_enum_baked<E: Copy>(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
    field: &'static str,
    default: E,
    table: EnumTable<E>,
) -> Result<E, FactoryError> {
    let Some(v) = fields.get(field) else {
        return Ok(default);
    };
    if let Some(p) = v.as_str().and_then(|s| match spec::FieldRef::classify(s) {
        spec::FieldRef::Param(p) => Some(p),
        _ => None,
    }) {
        let decl = ctx
            .params
            .get(p)
            .ok_or_else(|| FactoryError::UnknownParam(p.to_string()))?;
        let value = check_enum_param(field, p, decl, table)?;
        ctx.warn(format!(
            "field `{field}`: `${p}` is read when the graph is built, so its declared \
             default ({}) is baked in and render-time overrides of `{p}` will not change \
             this field",
            decl.default,
        ));
        return Ok(value);
    }
    enum_literal(field, v, table)
}

/// Parse a caller-supplied parameter assignment (CLI `--param k=v`,
/// server query string) against the document's declarations. Unknown
/// names, type mismatches, and out-of-range numbers are errors — hosts
/// report them up front instead of silently clamping at eval time.
pub fn parse_param_value(
    decls: &indexmap::IndexMap<String, spec::ParamDecl>,
    name: &str,
    raw: &str,
) -> Result<ScalarValue, String> {
    let decl = decls
        .get(name)
        .ok_or_else(|| format!("unknown param `{name}`"))?;
    match decl.kind {
        spec::ParamKind::Number => {
            let v: f64 = raw
                .parse()
                .map_err(|_| format!("param `{name}`: `{raw}` is not a number"))?;
            if let Some(m) = decl.min {
                if v < m {
                    return Err(format!("param `{name}`: {v} is below min {m}"));
                }
            }
            if let Some(m) = decl.max {
                if v > m {
                    return Err(format!("param `{name}`: {v} is above max {m}"));
                }
            }
            Ok(ScalarValue::Number(v))
        }
        spec::ParamKind::Bool => match raw {
            "true" | "1" => Ok(ScalarValue::Bool(true)),
            "false" | "0" => Ok(ScalarValue::Bool(false)),
            _ => Err(format!("param `{name}`: expected true/false, got `{raw}`")),
        },
        spec::ParamKind::Color => spec::parse_hex_color(raw)
            .map(ScalarValue::Color)
            .ok_or_else(|| format!("param `{name}`: `{raw}` is not a `#rrggbb[aa]` color")),
        spec::ParamKind::String => {
            if let Some(choices) = &decl.choices {
                if !choices.iter().any(|c| c == raw) {
                    return Err(format!(
                        "param `{name}`: `{raw}` is not one of {}",
                        choices.join(", ")
                    ));
                }
            }
            Ok(ScalarValue::String(raw.into()))
        }
    }
}

/// What an [`InReader`] accumulated: scalar input ports (to append
/// after the node's fixed ports), their connections, and the names of
/// `$param` references (returned from `Node::param_refs`).
#[derive(Debug, Default)]
pub struct InParts {
    pub ports: Vec<PortSpec>,
    pub connections: Vec<Connection>,
    pub param_refs: Vec<String>,
}

/// Field reader used by node factories: classifies each field as
/// literal / `$param` / `@node` and accumulates the node's scalar
/// port plumbing. See the module docs for the usage pattern.
pub struct InReader<'a, 'c> {
    fields: &'a serde_json::Map<String, Value>,
    ctx: &'a FactoryCtx<'c>,
    parts: InParts,
    next_port: usize,
}

impl<'a, 'c> InReader<'a, 'c> {
    /// `fixed_ports` is the number of input ports the node declares
    /// before any scalar ports (e.g. 1 for `blur`'s `input`). Scalar
    /// port indices start there.
    pub fn new(
        fields: &'a serde_json::Map<String, Value>,
        ctx: &'a FactoryCtx<'c>,
        fixed_ports: usize,
    ) -> Self {
        Self {
            fields,
            ctx,
            parts: InParts::default(),
            next_port: fixed_ports,
        }
    }

    /// Required number field.
    pub fn number(&mut self, name: &'static str) -> Result<In<f64>, FactoryError> {
        self.read(name, None)
    }

    /// Optional number field with a default.
    pub fn number_or(&mut self, name: &'static str, default: f64) -> Result<In<f64>, FactoryError> {
        self.read(name, Some(default))
    }

    /// Required color field (`#rrggbb[aa]`, `$param`, or `@node`).
    pub fn color(&mut self, name: &'static str) -> Result<In<[f32; 4]>, FactoryError> {
        self.read(name, None)
    }

    /// Optional color field with a default.
    pub fn color_or(
        &mut self,
        name: &'static str,
        default: [f32; 4],
    ) -> Result<In<[f32; 4]>, FactoryError> {
        self.read(name, Some(default))
    }

    /// Optional color field; `None` when absent.
    pub fn color_opt(&mut self, name: &'static str) -> Result<Option<In<[f32; 4]>>, FactoryError> {
        if !self.fields.contains_key(name) {
            return Ok(None);
        }
        Ok(Some(self.read(name, None)?))
    }

    /// Optional bool field with a default.
    pub fn bool_or(&mut self, name: &'static str, default: bool) -> Result<In<bool>, FactoryError> {
        self.read(name, Some(default))
    }

    /// Optional field taking one of a fixed set of names: a literal name,
    /// or a `$param` of kind `string` resolved at every eval. The param's
    /// default and every value its `enum` allows must be names the field
    /// accepts, checked here so a bad declaration fails the build.
    pub fn enum_or<E: Copy + PartialEq>(
        &mut self,
        name: &'static str,
        default: E,
        table: EnumTable<E>,
    ) -> Result<InEnum<E>, FactoryError> {
        let src = match self.fields.get(name) {
            None => EnumSrc::Const(default),
            Some(v) => match v.as_str().map(spec::FieldRef::classify) {
                Some(spec::FieldRef::Param(p)) => {
                    let decl = self
                        .ctx
                        .params
                        .get(p)
                        .ok_or_else(|| FactoryError::UnknownParam(p.to_string()))?;
                    let fallback = check_enum_param(name, p, decl, table)?;
                    self.parts.param_refs.push(p.to_string());
                    EnumSrc::Param {
                        name: p.to_string(),
                        fallback,
                    }
                }
                _ => EnumSrc::Const(enum_literal(name, v, table)?),
            },
        };
        Ok(InEnum {
            table,
            field: name,
            src,
        })
    }

    /// Read a scalar that lives *inside* a composite field — one stop of
    /// a colour ramp, one entry of a palette, one number of a dash
    /// pattern. `label` names the position for error messages
    /// (`"stops[2].color"`).
    ///
    /// Accepts a literal or a `$param`, but not a `@node`: a port needs
    /// a `&'static str` name and its own slot in `Node::inputs()`, and a
    /// table of unknown length can offer neither. Resolve the whole
    /// table once per eval, never per pixel.
    pub fn nested<T: ScalarType>(&mut self, label: &str, v: &Value) -> Result<In<T>, FactoryError> {
        if let Some(s) = v.as_str() {
            match spec::FieldRef::classify(s) {
                spec::FieldRef::Node(_) => {
                    return Err(FactoryError::BadField {
                        field: label.to_string(),
                        msg: format!(
                            "expected {} literal or `$param`, got a `@node` ref — a scalar \
                             port cannot be wired into one entry of a table",
                            T::NAME
                        ),
                    })
                }
                spec::FieldRef::Param(p) => return self.param_in(label, p),
                spec::FieldRef::Literal(_) => {} // fall through
            }
        }
        T::from_json(v)
            .map(In::Const)
            .ok_or_else(|| FactoryError::BadField {
                field: label.to_string(),
                msg: format!("expected {} literal or `$param`", T::NAME),
            })
    }

    /// Build a `$param`-backed `In` and record the reference, checking
    /// the declaration's type and default. Shared by whole-field and
    /// nested reads; `label` is only for error messages.
    fn param_in<T: ScalarType>(&mut self, label: &str, p: &str) -> Result<In<T>, FactoryError> {
        let decl = self
            .ctx
            .params
            .get(p)
            .ok_or_else(|| FactoryError::UnknownParam(p.to_string()))?;
        if !T::matches_kind(decl.kind) {
            return Err(FactoryError::BadField {
                field: label.to_string(),
                msg: format!(
                    "param `${p}` is declared `{:?}`, but this field needs a {}",
                    decl.kind,
                    T::NAME
                ),
            });
        }
        let fallback = T::from_json(&decl.default).ok_or_else(|| FactoryError::BadField {
            field: label.to_string(),
            msg: format!("param `${p}` default is not a valid {}", T::NAME),
        })?;
        self.parts.param_refs.push(p.to_string());
        Ok(In::Param {
            name: p.to_string(),
            fallback,
            min: decl.min,
            max: decl.max,
        })
    }

    fn read<T: ScalarType>(
        &mut self,
        name: &'static str,
        default: Option<T>,
    ) -> Result<In<T>, FactoryError> {
        let Some(v) = self.fields.get(name) else {
            return default
                .map(In::Const)
                .ok_or_else(|| FactoryError::MissingField(name.to_string()));
        };
        self.element(name, v)
    }

    /// Read a scalar that is not a top-level field but stands on its own,
    /// as one condition of a list does: a literal, a `$param`, or a `@node`,
    /// which becomes a scalar port named `name`. Unlike
    /// [`nested`](Self::nested) it can take a port, and so needs a
    /// `&'static` name for it.
    pub fn element<T: ScalarType>(
        &mut self,
        name: &'static str,
        v: &Value,
    ) -> Result<In<T>, FactoryError> {
        if let Some(s) = v.as_str() {
            match spec::FieldRef::classify(s) {
                spec::FieldRef::Node(id) => {
                    let ix = self.next_port;
                    self.next_port += 1;
                    self.parts.ports.push(PortSpec {
                        name,
                        accepts: ACCEPTS_SCALAR,
                        optional: false,
                    });
                    self.parts.connections.push(Connection {
                        port: name.to_string(),
                        src: id.to_string(),
                    });
                    return Ok(In::Port { ix, name });
                }
                spec::FieldRef::Param(p) => return self.param_in(name, p),
                spec::FieldRef::Literal(_) => {} // fall through
            }
        }
        T::from_json(v)
            .map(In::Const)
            .ok_or_else(|| FactoryError::BadField {
                field: name.into(),
                // A JSON array here is almost always a MapLibre expression
                // aimed at the wrong field: paint properties that accept one
                // keep it on a `<name>-expr` sibling, since an expression is
                // evaluated per feature rather than wired as a port.
                msg: if v.is_array() {
                    format!(
                        "expected {} literal, `$param`, or `@node`, got a JSON array — \
                         if that is a MapLibre expression, it belongs on `{name}-expr`",
                        T::NAME
                    )
                } else {
                    format!("expected {} literal, `$param`, or `@node`", T::NAME)
                },
            })
    }

    /// Hand back the accumulated ports / connections / param refs.
    pub fn finish(self) -> InParts {
        self.parts
    }
}

/// A numeric field whose value decides how much canvas padding the graph
/// needs — a blur's sigma, a morphology radius, a warp amplitude.
///
/// Padding is fixed before evaluation starts, so the build has to know an
/// upper bound for these. A literal or a `$param` with a declared `max`
/// carries one ([`In::static_bound`]); an `@node` port does not, and used
/// to be rejected outright — which left no way to drive a blur from a
/// `math` chain, since a computed value cannot be a param.
///
/// So the bound becomes something the style can state on its own, as a
/// literal `<field>-max` sibling. Padding is computed from that, and the
/// value the port produces is clamped to it at render time: the canvas
/// cannot grow mid-render, so a larger value would read past the margin
/// and clamp the tile's edge pixels instead. Clamping is reported once
/// per node — quietly weakening a filter the style asked for is worth a
/// line in the log, but not one per tile.
pub struct PaddingIn {
    value: In<f64>,
    bound: f64,
    field: &'static str,
    clamped: std::sync::atomic::AtomicBool,
}

impl PaddingIn {
    /// Read a required padding-determining field.
    pub fn read(
        r: &mut InReader<'_, '_>,
        fields: &serde_json::Map<String, Value>,
        field: &'static str,
    ) -> Result<Self, FactoryError> {
        let value = r.number(field)?;
        Self::from_value(value, fields, field)
    }

    /// Read an optional padding-determining field with a default.
    pub fn read_or(
        r: &mut InReader<'_, '_>,
        fields: &serde_json::Map<String, Value>,
        field: &'static str,
        default: f64,
    ) -> Result<Self, FactoryError> {
        let value = r.number_or(field, default)?;
        Self::from_value(value, fields, field)
    }

    /// Pair an already-read `In` with its bound — for fields whose value
    /// comes from a fallback chain (`amp-px` seeding `amp-x-px`).
    pub fn from_value(
        value: In<f64>,
        fields: &serde_json::Map<String, Value>,
        field: &'static str,
    ) -> Result<Self, FactoryError> {
        let ceiling = format!("{field}-max");
        let declared = fields.get(&ceiling).and_then(Value::as_f64);
        let bound = match (value.static_bound(), declared) {
            // A declared ceiling wins even over a literal: it is the
            // author saying "no more than this", and clamping to it is
            // then a no-op for a literal within range.
            (_, Some(d)) => d,
            (Some(b), None) => b,
            (None, None) => {
                return Err(FactoryError::BadField {
                    field: field.to_string(),
                    msg: format!(
                        "canvas padding is fixed before rendering, so `{field}` needs an \
                         upper bound at build time: use a literal, a `$param` with `max`, \
                         or declare `{ceiling}` alongside the `@node` port"
                    ),
                })
            }
        };
        Ok(Self {
            value,
            bound,
            field,
            clamped: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// The bound padding was computed from.
    pub fn bound(&self) -> f64 {
        self.bound
    }

    /// The value for this render, clamped to [`PaddingIn::bound`].
    pub fn get(
        &self,
        ctx: &crate::EvalCtx<'_>,
        inputs: &[Option<crate::PortValue>],
    ) -> Result<f64, crate::EvalError> {
        let raw = self.value.get(ctx, inputs)?;
        if raw > self.bound {
            if !self
                .clamped
                .swap(true, std::sync::atomic::Ordering::Relaxed)
            {
                tracing::warn!(
                    "`{}`: {raw} exceeds the {} the canvas was padded for; clamping. \
                     Raise `{}-max` to let it through.",
                    self.field,
                    self.bound,
                    self.field,
                );
            }
            return Ok(self.bound);
        }
        Ok(raw)
    }

    pub fn param_hash(&self, h: &mut xxhash_rust::xxh3::Xxh3) {
        self.value.param_hash(h);
        h.update(&self.bound.to_le_bytes());
    }

    pub fn param_refs(&self) -> Vec<String> {
        match &self.value {
            In::Param { name, .. } => vec![name.clone()],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Warnings;

    fn decls(json: &str) -> indexmap::IndexMap<String, spec::ParamDecl> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_string_param_takes_any_text_without_an_enum() {
        let d = decls(r#"{ "label": { "type": "string", "default": "x" } }"#);
        assert_eq!(
            parse_param_value(&d, "label", "anything at all"),
            Ok(ScalarValue::String("anything at all".into()))
        );
    }

    #[test]
    fn a_string_param_with_an_enum_rejects_other_values() {
        let d = decls(
            r#"{ "mode": { "type": "string", "enum": ["multiply", "screen"], "default": "screen" } }"#,
        );
        assert_eq!(
            parse_param_value(&d, "mode", "multiply"),
            Ok(ScalarValue::String("multiply".into()))
        );
        let err = parse_param_value(&d, "mode", "overlay").unwrap_err();
        assert!(err.contains("not one of multiply, screen"), "{err}");
    }

    #[test]
    fn an_enum_must_hold_its_default_and_belong_to_a_string() {
        let d = decls(
            r#"{
              "a": { "type": "string", "enum": ["x", "y"], "default": "z" },
              "b": { "type": "number", "enum": ["x"], "default": 1 },
              "c": { "type": "string", "enum": [], "default": "x" },
              "d": { "type": "string", "default": 3 },
              "e": { "type": "string", "enum": ["x", "y"], "default": "y" }
            }"#,
        );
        assert!(d["a"]
            .check()
            .unwrap_err()
            .contains("not one of its `enum`"));
        assert!(d["b"].check().unwrap_err().contains("only for `string`"));
        assert!(d["c"].check().unwrap_err().contains("no values"));
        assert!(d["d"].check().unwrap_err().contains("not a string"));
        assert_eq!(d["e"].check(), Ok(()));
    }

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Cap {
        Butt,
        Round,
    }
    const CAPS: EnumTable<Cap> = &[("butt", Cap::Butt), ("round", Cap::Round)];

    fn read_cap(
        params: &str,
        field: serde_json::Value,
    ) -> Result<(InEnum<Cap>, Vec<String>), FactoryError> {
        let params = decls(params);
        let sources = indexmap::IndexMap::new();
        let warnings = Warnings::new();
        let ctx = FactoryCtx {
            params: &params,
            sources: &sources,
            node: "n",
            warnings: &warnings,
        };
        let fields = serde_json::json!({ "cap": field });
        let mut r = InReader::new(fields.as_object().unwrap(), &ctx, 0);
        let cap = r.enum_or("cap", Cap::Butt, CAPS)?;
        Ok((cap, r.finish().param_refs))
    }

    #[test]
    fn an_enum_field_reads_a_literal_or_a_string_param() {
        let (cap, refs) = read_cap("{}", "round".into()).unwrap();
        assert_eq!(cap.fallback(), Cap::Round);
        assert!(refs.is_empty());

        let (cap, refs) = read_cap(
            r#"{ "c": { "type": "string", "enum": ["butt", "round"], "default": "round" } }"#,
            "$c".into(),
        )
        .unwrap();
        assert_eq!(cap.fallback(), Cap::Round);
        assert_eq!(cap.param_name(), Some("c"));
        assert_eq!(refs, ["c"]);
    }

    #[test]
    fn an_enum_field_rejects_a_param_it_cannot_follow() {
        let msg =
            |params: &str, field: &str| read_cap(params, field.into()).unwrap_err().to_string();
        // Allows a value the field does not know.
        let m = msg(
            r#"{ "c": { "type": "string", "enum": ["butt", "square"], "default": "butt" } }"#,
            "$c",
        );
        assert!(m.contains("allows `square`"), "{m}");
        // Not a string param.
        let m = msg(r#"{ "c": { "type": "number", "default": 1 } }"#, "$c");
        assert!(m.contains("declared `number`"), "{m}");
        // A default the field does not know.
        let m = msg(
            r#"{ "c": { "type": "string", "default": "square" } }"#,
            "$c",
        );
        assert!(m.contains("defaults to `square`"), "{m}");
        // A node cannot produce a string.
        let m = msg("{}", "@other");
        assert!(m.contains("no node produces a string"), "{m}");
        // An unknown literal.
        let m = msg("{}", "square");
        assert!(m.contains("unknown cap `square`"), "{m}");
    }
}
