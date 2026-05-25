//! `brush-file` — `() -> Brush`. Resolves a brush via the host's
//! [`AssetLoader`](ezu_graph::AssetLoader).

use ezu_graph::{
    schema_frag, Asset, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue,
};
use ezu_style as spec;
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

struct BrushFileNode {
    src: String,
}

impl Node for BrushFileNode {
    fn op_name(&self) -> &'static str {
        "brush-file"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Brush
    }
    fn eval(&self, ctx: &EvalCtx<'_>, _: &[Option<PortValue>]) -> Result<PortValue, EvalError> {
        let asset = ctx.assets.load(&self.src)?;
        let Asset::Brush(b) = asset else {
            return Err(EvalError::Other(format!(
                "asset `{}` is not a brush",
                self.src
            )));
        };
        Ok(PortValue::Brush(b))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"brush-file");
        h.update(self.src.as_bytes());
    }
}

pub(super) struct BrushFileFactory;
impl NodeFactory for BrushFileFactory {
    fn op_name(&self) -> &'static str {
        "brush-file"
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
        // `@name` -> look up in document `sources` and use that entry's
        // src. Otherwise the literal is passed straight to the loader.
        let src = match spec::FieldRef::classify(raw) {
            spec::FieldRef::Node(name) => {
                let source = ctx
                    .sources
                    .get(name)
                    .ok_or_else(|| FactoryError::UnknownAsset(name.to_string()))?;
                let spec::SourceDecl::Brush(file) = source else {
                    return Err(FactoryError::BadField {
                        field: "src".into(),
                        msg: format!("source `{name}` is not a brush"),
                    });
                };
                file.src.clone()
            }
            spec::FieldRef::Literal(s) => s.to_string(),
            spec::FieldRef::Param(_) => {
                return Err(FactoryError::BadField {
                    field: "src".into(),
                    msg: "param refs not allowed for brush src".into(),
                })
            }
        };
        Ok(BuiltNode {
            node: Box::new(BrushFileNode { src }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Brush source. `src` is an `@asset` ref or a literal path/name resolved by the host's AssetLoader.",
            "properties": { "src": schema_frag::asset_ref() },
            "required": ["src"],
        })
    }
}

ezu_graph::submit_node!(BrushFileFactory);
