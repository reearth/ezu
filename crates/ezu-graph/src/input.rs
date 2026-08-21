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
//! let sigma = r.number(\"sigma\")?;          // In<f64>
//! let color = r.color_or(\"color\", WHITE)?; // In<[f32; 4]>
//! let parts = r.finish();
//! // node stores `sigma`, `color`, `parts.ports` (appended after the
//! // fixed ports), returns `parts.param_refs` from `Node::param_refs`,
//! // and the factory returns `parts.connections` alongside any fixed
//! // ones.
//! ```
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
    fn from_scalar(v: ScalarValue) -> Option<Self>;
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
    fn from_scalar(v: ScalarValue) -> Option<Self> {
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
    fn from_scalar(v: ScalarValue) -> Option<Self> {
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
    fn from_scalar(v: ScalarValue) -> Option<Self> {
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
                    Some(sv) => T::from_scalar(sv).ok_or_else(|| {
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
                T::from_scalar(*sv).ok_or_else(|| {
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
                spec::FieldRef::Param(p) => {
                    let decl = self
                        .ctx
                        .params
                        .get(p)
                        .ok_or_else(|| FactoryError::UnknownParam(p.to_string()))?;
                    if !T::matches_kind(decl.kind) {
                        return Err(FactoryError::BadField {
                            field: name.into(),
                            msg: format!(
                                "param `${p}` is declared `{:?}`, but this field needs a {}",
                                decl.kind,
                                T::NAME
                            ),
                        });
                    }
                    let fallback =
                        T::from_json(&decl.default).ok_or_else(|| FactoryError::BadField {
                            field: name.into(),
                            msg: format!("param `${p}` default is not a valid {}", T::NAME),
                        })?;
                    self.parts.param_refs.push(p.to_string());
                    return Ok(In::Param {
                        name: p.to_string(),
                        fallback,
                        min: decl.min,
                        max: decl.max,
                    });
                }
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
