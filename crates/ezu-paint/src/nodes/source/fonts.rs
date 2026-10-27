//! `fonts` — `() -> Fonts`. Load the style's `font` and `glyphs` sources
//! into named font stacks, for `text` and `text-labels` to pick from.
//!
//! Each stack is a fallback chain: the first face covering a character
//! shapes it. Outline fonts and SDF glyph stacks mix freely within one. A
//! glyph stack is carried as the object the host loaded, fetcher and all,
//! so ranges a label needs are still fetched lazily, per codepoint, as the
//! label is laid out.
//!
//! Which stack each feature's label uses is styling, and stays on the
//! label node (`font`, `font-expr`); which faces a stack name stands for
//! decides what is loaded, so it lives here.
//!
//! Also home to what loading a stack takes ([`font_source_key`],
//! [`load_stack`]), which the label nodes' deprecated `font` / `font-stacks`
//! fields still use.

use std::collections::HashMap;
use std::sync::Arc;

use ezu_core::text::{Font, SdfFontStack, StackEntry};
use ezu_graph::{
    Asset, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory, PortKind,
    PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

/// Canonical name of a font stack: names trimmed, joined with `,` (no
/// space). Matches `maplibre_expr`'s `FormatSection::font_stack`, so a
/// `format` section's stack, a `font-expr` result and a declared stack name
/// look up identically.
pub(crate) fn stack_key<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names.map(str::trim).collect::<Vec<_>>().join(",")
}

/// Resolve one `font` / `glyphs` source name (bare or `@`-prefixed) to its
/// asset key: a font's `url`, a glyphs source's `{range}` URL template.
/// `field` names the field in error messages.
pub(crate) fn font_source_key(
    ctx: &FactoryCtx<'_>,
    field: &str,
    name: &str,
) -> Result<String, FactoryError> {
    let name = name.strip_prefix('@').unwrap_or(name);
    match ctx.sources.get(name) {
        Some(ezu_style::SourceDecl::Font(f)) => Ok(f.url.clone()),
        Some(ezu_style::SourceDecl::Glyphs(g)) => Ok(g.asset_key()),
        Some(_) => Err(FactoryError::BadField {
            field: field.into(),
            msg: format!("source `{name}` is not a font or glyphs source"),
        }),
        None => Err(FactoryError::UnknownAsset(name.to_string())),
    }
}

/// Load one stack of font asset keys. Outline fonts and SDF glyph stacks
/// mix; the draw path is picked per glyph by its backend.
pub(crate) fn load_stack(ctx: &EvalCtx<'_>, keys: &[String]) -> Result<Vec<StackEntry>, EvalError> {
    let mut fonts: Vec<StackEntry> = Vec::with_capacity(keys.len());
    for key in keys {
        let entry = match ctx.assets.load(key)? {
            Asset::Font(opq) => StackEntry::Outline(
                opq.downcast::<Font>()
                    .map_err(|_| EvalError::Other(format!("`{key}` payload is not a text Font")))?,
            ),
            Asset::Glyphs(opq) => {
                StackEntry::Sdf(opq.downcast::<SdfFontStack>().map_err(|_| {
                    EvalError::Other(format!("`{key}` payload is not an SdfFontStack"))
                })?)
            }
            _ => {
                return Err(EvalError::Other(format!(
                    "asset `{key}` is not a font or glyphs source"
                )))
            }
        };
        fonts.push(entry);
    }
    Ok(fonts)
}

/// The value on a `fonts` port: named font stacks, in the order the
/// `fonts` node's `font-stacks` object yields them (by name). That order is
/// each stack's stable index, which a label node turns into the id its
/// layout cache and collision tie-break use, so every tile built from one
/// style agrees on it.
pub(crate) struct Fonts {
    stacks: Vec<(String, Vec<StackEntry>)>,
    by_name: HashMap<String, usize>,
}

impl Fonts {
    /// Every stack with its name, in index order.
    pub(crate) fn stacks(&self) -> impl Iterator<Item = (&str, &[StackEntry])> {
        self.stacks.iter().map(|(n, s)| (n.as_str(), s.as_slice()))
    }

    /// The stack named `name` (canonical, see [`stack_key`]).
    pub(crate) fn stack(&self, name: &str) -> Option<&[StackEntry]> {
        self.by_name.get(name).map(|&i| self.stacks[i].1.as_slice())
    }
}

/// Read a `font-stacks` object: stack name → the asset keys of its
/// sources, in the order the object yields them, which is by name.
pub(crate) fn read_font_stacks(
    ctx: &FactoryCtx<'_>,
    v: &Value,
) -> Result<Vec<(String, Vec<String>)>, FactoryError> {
    let Value::Object(map) = v else {
        return Err(FactoryError::BadField {
            field: "font-stacks".into(),
            msg: "expected an object of stack name → source-name array".into(),
        });
    };
    let mut out = Vec::with_capacity(map.len());
    for (key, names) in map {
        let arr = names.as_array().ok_or_else(|| FactoryError::BadField {
            field: "font-stacks".into(),
            msg: format!("stack `{key}` must be an array of source names"),
        })?;
        let mut keys = Vec::with_capacity(arr.len());
        for v in arr {
            let name = v.as_str().ok_or_else(|| FactoryError::BadField {
                field: "font-stacks".into(),
                msg: format!("stack `{key}` entries must be strings"),
            })?;
            keys.push(font_source_key(ctx, "font-stacks", name)?);
        }
        out.push((key.clone(), keys));
    }
    Ok(out)
}

struct FontsNode {
    /// Canonical stack name → asset keys, in index order.
    stacks: Vec<(String, Vec<String>)>,
}

impl Node for FontsNode {
    fn op_name(&self) -> &'static str {
        "fonts"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Fonts
    }
    fn eval(&self, ctx: &EvalCtx<'_>, _: &[Option<PortValue>]) -> Result<PortValue, EvalError> {
        let mut stacks = Vec::with_capacity(self.stacks.len());
        let mut by_name = HashMap::with_capacity(self.stacks.len());
        for (i, (name, keys)) in self.stacks.iter().enumerate() {
            stacks.push((name.clone(), load_stack(ctx, keys)?));
            by_name.entry(name.clone()).or_insert(i);
        }
        Ok(PortValue::Fonts(Arc::new(Fonts { stacks, by_name })))
    }
    fn asset_inputs(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .stacks
            .iter()
            .flat_map(|(_, keys)| keys.iter().cloned())
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"fonts");
        for (name, keys) in &self.stacks {
            h.update(name.as_bytes());
            h.update(&[0]);
            for k in keys {
                h.update(k.as_bytes());
                h.update(&[0]);
            }
            h.update(&[1]);
        }
    }
}

pub(super) struct FontsFactory;
impl NodeFactory for FontsFactory {
    fn op_name(&self) -> &'static str {
        "fonts"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let v = fields
            .get("font-stacks")
            .ok_or_else(|| FactoryError::MissingField("font-stacks".into()))?;
        let stacks = read_font_stacks(ctx, v)?
            .into_iter()
            .map(|(name, keys)| (stack_key(name.split(',')), keys))
            .collect();
        Ok(BuiltNode {
            node: Box::new(FontsNode { stacks }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "The style's fonts, as named font stacks for `text` and `text-labels` (wired to their `fonts`). Each stack is a fallback chain of `font` and `glyphs` sources: the first covering a character shapes it, and outline fonts and SDF glyph stacks mix freely. Glyph ranges are fetched as labels need them. A label node names the stack its labels use in `font` (or per feature in `font-expr`).",
            "properties": {
                "font-stacks": { "type": "object",
                                 "additionalProperties": { "type": "array", "items": { "type": "string" } },
                                 "description": "Stack name → ordered `font` / `glyphs` source names. A name of several fonts is written as MapLibre's `text-font` joins them, with `,` (spaces around it are ignored), so a `font-expr` result or a `format` section's `text-font` finds it. Stacks are numbered in name order, and the number is each stack's stable id, which the layout cache and the collision tie-break use." },
            },
            "required": ["font-stacks"],
        })
    }
}

ezu_graph::submit_node!(FontsFactory);
