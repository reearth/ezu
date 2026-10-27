//! `icon` — `() -> Sprite`. Crop one named icon out of a `sprite`
//! source's atlas and emit it as a `Sprite` port value.
//!
//! The companion of [`image`](super::image) for sprite sheets: where
//! `image` returns a whole image, `icon` returns a single named
//! sub-rectangle. The cropped buffer is returned at its atlas pixel
//! size; downstream `stamp` (icons) / `tiling` (`fill-pattern`) place
//! it onto the canvas. To pick an icon per feature instead, wire a
//! [`sprite`](super::sprite) node's whole sheet into the op that draws.
//!
//! A name the sheet lacks yields a 1×1 transparent sprite, which draws
//! nothing, rather than failing the tile, so one bad name in a large
//! symbol layer doesn't blank the render. It is reported once, as every
//! reader of a sheet reports one (see [`super::sprite::IconCrops`]).

use std::sync::Arc;

use ezu_graph::{
    schema_frag, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use super::sprite::{load_sheet, sprite_atlas_key, IconCrops};

struct IconNode {
    /// The sprite atlas asset key (the source's `image` src).
    sprite: String,
    name: String,
    crops: IconCrops,
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
        let sheet = load_sheet(ctx, &self.sprite)?;
        let cropped = self
            .crops
            .crop("icon", &sheet, &self.name)
            .unwrap_or_else(|| RasterBuf::new(1, 1));
        Ok(PortValue::Sprite(Arc::new(cropped)))
    }
    fn asset_inputs(&self) -> Vec<String> {
        vec![self.sprite.clone()]
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
        let sprite = sprite_atlas_key("sprite", raw, ctx)?;
        let name = fields
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| FactoryError::MissingField("name".into()))?
            .to_string();
        Ok(BuiltNode {
            node: Box::new(IconNode {
                sprite,
                name,
                crops: IconCrops::default(),
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Crop one named icon from a `sprite` source's atlas. `sprite` is an `@sprite-source` ref (or literal atlas key); `name` is the icon's index key. Output is a Sprite at the icon's atlas pixel size; a name the sheet lacks gives a transparent 1×1 sprite, which draws nothing. To pick an icon per feature, wire a `sprite` node's sheet into `stamp`, `line-stamp` or `text` instead.",
            "properties": {
                "sprite": schema_frag::asset_ref(),
                "name": { "type": "string" },
            },
            "required": ["sprite", "name"],
        })
    }
}

ezu_graph::submit_node!(IconFactory);
