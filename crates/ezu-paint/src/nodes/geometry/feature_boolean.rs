//! `feature-boolean` — `(Features, Features) -> Features`. Polygon
//! set operations between two Features inputs. Lines and points are
//! dropped — booleans are defined on polygons.
//!
//! Because it merges polygons across both inputs, the output is a single
//! group with no per-feature properties.

use ezu_features::ops::boolean::{polygon_boolean, BoolOp};
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EnumTable, EvalCtx, EvalError,
    FactoryCtx, FactoryError, InEnum, InReader, InfluenceCtx, Node, NodeFactory, PortKind,
    PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, FeatureGroup};

/// `xor` is another name for `symmetric-difference`.
const MODES: EnumTable<BoolOp> = &[
    ("union", BoolOp::Union),
    ("intersection", BoolOp::Intersection),
    ("difference", BoolOp::Difference),
    ("symmetric-difference", BoolOp::SymmetricDifference),
    ("xor", BoolOp::SymmetricDifference),
];

struct FeatureBooleanNode {
    op: InEnum<BoolOp>,
    param_refs: Vec<String>,
}

impl Node for FeatureBooleanNode {
    fn op_name(&self) -> &'static str {
        "feature-boolean"
    }

    /// Nothing upstream of this op may be dropped.
    ///
    /// A union carries the other input's geometry through, so this
    /// input's own bounds do not bound the output.
    fn influence_pad(&self, _ctx: &InfluenceCtx<'_>) -> u32 {
        InfluenceCtx::UNBOUNDED
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[
            PortSpec {
                name: "a",
                accepts: &[PortKind::Features],
                optional: false,
            },
            PortSpec {
                name: "b",
                accepts: &[PortKind::Features],
                optional: false,
            },
        ];
        SPECS
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Features
    }
    fn coord_space(&self) -> CoordSpace {
        CoordSpace::Tile
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let op = self.op.get(ctx)?;
        let a = downcast_features(
            inputs[0]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("a".into()))?,
        )?;
        let b = downcast_features(
            inputs[1]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("b".into()))?,
        )?;
        let a_polys: Vec<_> = a.polygons().cloned().collect();
        let b_polys: Vec<_> = b.polygons().cloned().collect();
        let polygons = polygon_boolean(&a_polys, &b_polys, op);
        if polygons.is_empty() {
            return Ok(features_value(a.extent, vec![]));
        }
        Ok(features_value(
            a.extent,
            vec![FeatureGroup::synthetic(polygons, vec![], vec![])],
        ))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"feature-boolean");
        self.op.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct FeatureBooleanFactory;
impl NodeFactory for FeatureBooleanFactory {
    fn op_name(&self) -> &'static str {
        "feature-boolean"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let a = take_input_ref(fields, "a")?;
        let b = take_input_ref(fields, "b")?;
        let mut r = InReader::new(fields, ctx, 2);
        let op = r.enum_or("mode", BoolOp::Union, MODES)?;
        Ok(BuiltNode {
            node: Box::new(FeatureBooleanNode {
                op,
                param_refs: r.finish().param_refs,
            }),
            connections: vec![
                Connection {
                    port: "a".into(),
                    src: a,
                },
                Connection {
                    port: "b".into(),
                    src: b,
                },
            ],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Polygon boolean operation between two Features inputs. Subject = `a`, clip = `b`. Lines and points on either input are dropped — booleans are defined on polygons only.",
            "properties": {
                "a": schema_frag::node_ref(),
                "b": schema_frag::node_ref(),
                "mode": schema_frag::in_enum(serde_json::json!({
                    "type": "string",
                    "enum": ezu_graph::enum_names(MODES),
                    "default": "union",
                })),
            },
            "required": ["a", "b"],
        })
    }
}

ezu_graph::submit_node!(FeatureBooleanFactory);
