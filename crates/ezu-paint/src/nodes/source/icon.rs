//! `icon` — `() -> Sprite`. Crop one named icon out of a `sprite`
//! source's atlas and emit it as a `Sprite` port value.
//!
//! The companion of [`image`](super::image) for sprite sheets: where
//! `image` returns a whole image, `icon` returns a single named
//! sub-rectangle. The cropped buffer is returned at its atlas pixel
//! size; downstream `stamp` (icons) / `tiling` (`fill-pattern`) place
//! it onto the canvas.
//!
//! A missing icon name yields a 1×1 transparent sprite (an invisible
//! no-op) rather than failing the tile, so one bad name in a large
//! symbol layer doesn't blank the render.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, Asset, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue, RasterBuf,
};
use ezu_style as spec;
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

struct IconNode {
    /// The sprite atlas asset key (the source's `image` src).
    sprite: String,
    name: String,
}

impl Node for IconNode {
    fn op_name(&self) -> &'static str {
        "icon"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Sprite
    }
    fn eval(&self, ctx: &EvalCtx<'_>, _: &[Option<PortValue>]) -> Result<PortValue, EvalError> {
        let asset = ctx.assets.load(&self.sprite)?;
        let Asset::Sprite(sheet) = asset else {
            return Err(EvalError::Other(format!(
                "asset `{}` is not a sprite sheet",
                self.sprite
            )));
        };
        let cropped = sheet
            .crop(&self.name)
            .unwrap_or_else(|| RasterBuf::new(1, 1));
        Ok(PortValue::Sprite(Arc::new(cropped)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"icon");
        h.update(self.sprite.as_bytes());
        h.update(&[0]);
        h.update(self.name.as_bytes());
    }
}

pub(super) struct IconFactory;
impl NodeFactory for IconFactory {
    fn op_name(&self) -> &'static str {
        "icon"
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
        // `@name` -> resolve to the sprite source's atlas `image` key; a
        // literal is treated as that key directly.
        let sprite = match spec::FieldRef::classify(raw) {
            spec::FieldRef::Node(name) => {
                let source = ctx
                    .sources
                    .get(name)
                    .ok_or_else(|| FactoryError::UnknownAsset(name.to_string()))?;
                let spec::SourceDecl::Sprite(sprite) = source else {
                    return Err(FactoryError::BadField {
                        field: "sprite".into(),
                        msg: format!("source `{name}` is not a sprite"),
                    });
                };
                sprite.image.clone()
            }
            spec::FieldRef::Literal(s) => s.to_string(),
            spec::FieldRef::Param(_) => {
                return Err(FactoryError::BadField {
                    field: "sprite".into(),
                    msg: "param refs not allowed for icon sprite".into(),
                });
            }
        };
        let name = fields
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| FactoryError::MissingField("name".into()))?
            .to_string();
        Ok(BuiltNode {
            node: Box::new(IconNode { sprite, name }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Crop one named icon from a `sprite` source's atlas. `sprite` is an `@sprite-source` ref (or literal atlas key); `name` is the icon's index key. Output is a Sprite at the icon's atlas pixel size.",
            "properties": {
                "sprite": schema_frag::asset_ref(),
                "name": { "type": "string" },
            },
            "required": ["sprite", "name"],
        })
    }
}

ezu_graph::submit_node!(IconFactory);
