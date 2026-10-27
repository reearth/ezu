//! `sprite` — `() -> SpriteSheet`. Load a `sprite` source's whole sheet, the
//! atlas with its name → rect index, for the ops that pick an icon per
//! feature: `stamp` and `line-stamp` (their `sheet` input) and `text` /
//! `text-labels` (`icon-sheet`).
//!
//! Also home to what every reader of a sheet shares, so the icon a name
//! gives cannot differ between them: [`sprite_atlas_key`] resolves a
//! `sprite` field to the sheet's asset key, and [`IconCrops`] crops named
//! icons out of a sheet. A name the sheet lacks draws no icon, wherever it
//! is asked for, and is reported once per node.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use ezu_graph::{
    schema_frag, take_input_ref, Asset, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf, SpriteSheet,
};
use ezu_style as spec;
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

/// Resolve a `sprite` field — an `@sprite-source` reference, or a literal
/// atlas key — to the sheet's asset key (the source's `image`). `field`
/// names the field in error messages.
pub(crate) fn sprite_atlas_key(
    field: &str,
    raw: &str,
    ctx: &FactoryCtx<'_>,
) -> Result<String, FactoryError> {
    match spec::FieldRef::classify(raw) {
        spec::FieldRef::Node(name) => {
            let source = ctx
                .sources
                .get(name)
                .ok_or_else(|| FactoryError::UnknownAsset(name.to_string()))?;
            let spec::SourceDecl::Sprite(sprite) = source else {
                return Err(FactoryError::BadField {
                    field: field.into(),
                    msg: format!("source `{name}` is not a sprite"),
                });
            };
            Ok(sprite.image.clone())
        }
        spec::FieldRef::Literal(s) => Ok(s.to_string()),
        spec::FieldRef::Param(_) => Err(FactoryError::BadField {
            field: field.into(),
            msg: "a sprite cannot come from a param".into(),
        }),
    }
}

/// Load the sheet bound at `key`.
pub(crate) fn load_sheet(ctx: &EvalCtx<'_>, key: &str) -> Result<Arc<SpriteSheet>, EvalError> {
    match ctx.assets.load(key)? {
        Asset::Sprite(sheet) => Ok(sheet),
        _ => Err(EvalError::Other(format!(
            "asset `{key}` is not a sprite sheet"
        ))),
    }
}

/// Crops named icons out of a sheet, remembering which names the sheet
/// lacked so each is reported once per node rather than once per tile.
///
/// A name with no rect in the sheet, or one whose rect falls outside the
/// atlas, draws no icon: `stamp` and `line-stamp` skip the features naming
/// it, a label keeps its text without the icon, and `icon` hands on a
/// transparent 1×1 sprite, which draws nothing either.
#[derive(Default)]
pub(crate) struct IconCrops {
    reported: Mutex<HashSet<String>>,
}

impl IconCrops {
    /// The icon `name` cut out of `sheet`, or `None` (reported) when the
    /// sheet has no such icon. `op` names the asking op in the report.
    pub(crate) fn crop(&self, op: &str, sheet: &SpriteSheet, name: &str) -> Option<RasterBuf> {
        let img = sheet.crop(name);
        if img.is_none() {
            let mut reported = self.reported.lock().unwrap_or_else(|e| e.into_inner());
            if reported.insert(name.to_string()) {
                tracing::warn!("{op}: the sprite sheet has no icon `{name}` — none is drawn");
            }
        }
        img
    }
}

/// Where an op that picks icons per feature gets its sprite sheet.
pub(crate) enum SheetInput {
    /// A `sprite` node wired to the op's input port at this index.
    Port(usize),
    /// The deprecated way: the op names the sprite source in a field and
    /// loads the sheet itself, holding this asset key.
    ///
    /// This is the anti-pattern the `Node` trait's "Sources read,
    /// processors process" warns against: the op repeats the `sprite`
    /// node's loading and must declare the asset itself. Kept so older
    /// styles draw as they did; the build warns.
    Field(String),
}

impl SheetInput {
    /// Read an op's sheet: its `port` field naming a `sprite` node, or its
    /// deprecated `legacy` field naming the sprite source. With both, the
    /// port wins. `port_ix` is the index the port takes among the op's
    /// inputs when wired. Returns `None` when neither is given; otherwise
    /// also the connection to add when the port is wired.
    pub(crate) fn read(
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
        port: &'static str,
        legacy: &'static str,
        port_ix: usize,
    ) -> Result<Option<(SheetInput, Option<Connection>)>, FactoryError> {
        let raw = match fields.get(legacy) {
            Some(v) => Some(v.as_str().ok_or_else(|| FactoryError::BadField {
                field: legacy.into(),
                msg: "expected an `@sprite-source` reference".into(),
            })?),
            None => None,
        };
        if fields.contains_key(port) {
            let src = take_input_ref(fields, port)?;
            if raw.is_some() {
                ctx.warn(format_args!(
                    "`{legacy}` is deprecated, and ignored: the sheet comes from `{port}`. \
                     Remove `{legacy}`"
                ));
            }
            return Ok(Some((
                SheetInput::Port(port_ix),
                Some(Connection {
                    port: port.into(),
                    src,
                }),
            )));
        }
        let Some(raw) = raw else {
            return Ok(None);
        };
        let key = sprite_atlas_key(legacy, raw, ctx)?;
        ctx.warn(format_args!(
            "`{legacy}` is deprecated: it makes the node load the sprite sheet itself. \
             Load it with a `sprite` node, {{ \"op\": \"sprite\", \"sprite\": \"{raw}\" }}, \
             and wire that node to `{port}` instead"
        ));
        Ok(Some((SheetInput::Field(key), None)))
    }

    /// The sheet for this eval.
    pub(crate) fn get(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
        port: &str,
    ) -> Result<Arc<SpriteSheet>, EvalError> {
        match self {
            SheetInput::Port(ix) => {
                let v = inputs
                    .get(*ix)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| EvalError::MissingInput(port.into()))?;
                v.as_sprite_sheet().cloned().ok_or_else(|| {
                    EvalError::Other(format!("port `{port}`: expected a sprite sheet"))
                })
            }
            SheetInput::Field(key) => load_sheet(ctx, key),
        }
    }

    /// The asset the deprecated field loads; none for a port.
    pub(crate) fn asset_inputs(&self) -> Option<String> {
        match self {
            SheetInput::Port(_) => None,
            SheetInput::Field(key) => Some(key.clone()),
        }
    }

    /// The deprecated field's asset key, which the op folds into its param
    /// hash as it always has; a port's sheet is keyed upstream.
    pub(crate) fn field_key(&self) -> Option<&str> {
        match self {
            SheetInput::Port(_) => None,
            SheetInput::Field(key) => Some(key),
        }
    }
}

/// The schema of a field wiring a `sprite` node to a sheet input.
pub(crate) fn sheet_schema(description: &str) -> Value {
    let mut v = schema_frag::node_ref();
    v["description"] = Value::from(description);
    v
}

/// The input port a sheet arrives on.
pub(crate) fn sheet_port(name: &'static str) -> PortSpec {
    PortSpec {
        name,
        accepts: &[PortKind::SpriteSheet],
        optional: false,
    }
}

struct SpriteNode {
    /// The sheet's asset key (the source's `image`).
    key: String,
}

impl Node for SpriteNode {
    fn op_name(&self) -> &'static str {
        "sprite"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::SpriteSheet
    }
    fn eval(&self, ctx: &EvalCtx<'_>, _: &[Option<PortValue>]) -> Result<PortValue, EvalError> {
        Ok(PortValue::SpriteSheet(load_sheet(ctx, &self.key)?))
    }
    fn asset_inputs(&self) -> Vec<String> {
        vec![self.key.clone()]
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"sprite");
        h.update(self.key.as_bytes());
    }
}

pub(super) struct SpriteFactory;
impl NodeFactory for SpriteFactory {
    fn op_name(&self) -> &'static str {
        "sprite"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let raw = fields
            .get("sprite")
            .and_then(Value::as_str)
            .ok_or_else(|| FactoryError::MissingField("sprite".into()))?;
        Ok(BuiltNode {
            node: Box::new(SpriteNode {
                key: sprite_atlas_key("sprite", raw, ctx)?,
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "A `sprite` source's whole sheet — its atlas and name → rect index — for the ops that pick an icon per feature by name: `stamp` and `line-stamp` take it on `sheet`, `text` and `text-labels` on `icon-sheet`. `sprite` is an `@sprite-source` ref (or literal atlas key). To place one named icon, use `icon` instead.",
            "properties": {
                "sprite": schema_frag::asset_ref(),
            },
            "required": ["sprite"],
        })
    }
}

ezu_graph::submit_node!(SpriteFactory);
