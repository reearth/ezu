//! `image` — `() -> Sprite`. Resolve an image asset via the host's
//! [`AssetLoader`](ezu_graph::AssetLoader) and emit its pixels as a
//! `Sprite` port value.
//!
//! The resolved buffer is returned **as-is**, at the asset's native
//! dimensions; consumers (`stamp` / `tiling` / `place`) are responsible
//! for placing it onto the canvas. The dedicated `Sprite` port kind
//! prevents `image` from being wired directly into a raster transform
//! or the document `output`, both of which assume a canvas-padded
//! raster.

use ezu_graph::{
    schema_frag, Asset, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue,
};
use ezu_style as spec;
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

struct ImageNode {
    src: String,
}

impl Node for ImageNode {
    fn op_name(&self) -> &'static str {
        "image"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self) -> PortKind {
        PortKind::Sprite
    }
    fn eval(&self, ctx: &EvalCtx<'_>, _: &[Option<PortValue>]) -> Result<PortValue, EvalError> {
        let asset = ctx.assets.load(&self.src)?;
        let Asset::Image(raster) = asset else {
            return Err(EvalError::Other(format!(
                "asset `{}` is not an image",
                self.src
            )));
        };
        Ok(PortValue::Sprite(raster))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"image");
        h.update(self.src.as_bytes());
    }
}

pub(super) struct ImageFactory;
impl NodeFactory for ImageFactory {
    fn op_name(&self) -> &'static str {
        "image"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let raw = fields
            .get("src")
            .and_then(Value::as_str)
            .ok_or_else(|| FactoryError::MissingField("src".into()))?;
        // `@name` -> look up in document `assets`; literal goes straight
        // to the loader.
        let src = match spec::FieldRef::classify(raw) {
            spec::FieldRef::Node(name) => {
                let asset = ctx
                    .assets
                    .get(name)
                    .ok_or_else(|| FactoryError::UnknownAsset(name.to_string()))?;
                if asset.kind != spec::AssetKind::Image {
                    return Err(FactoryError::BadField {
                        field: "src".into(),
                        msg: format!("asset `{name}` is not an image"),
                    });
                }
                asset.src.clone()
            }
            spec::FieldRef::Literal(s) => s.to_string(),
            spec::FieldRef::Param(_) => {
                return Err(FactoryError::BadField {
                    field: "src".into(),
                    msg: "param refs not allowed for image src".into(),
                });
            }
        };
        Ok(BuiltNode {
            node: Box::new(ImageNode { src }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Image asset source. `src` is an `@asset` ref or a literal path/name resolved by the host's AssetLoader. Output preserves the source dimensions.",
            "properties": { "src": schema_frag::asset_ref() },
            "required": ["src"],
        })
    }
}

ezu_graph::submit_node!(ImageFactory);
