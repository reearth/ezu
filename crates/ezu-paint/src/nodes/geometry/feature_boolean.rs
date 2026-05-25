//! `feature-boolean` — `(Features, Features) -> Features`. Polygon
//! set operations between two Features inputs. Lines and points are
//! dropped — booleans are defined on polygons.

use ezu_features::ops::boolean::{polygon_boolean, BoolOp};
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, read_optional_string};

struct FeatureBooleanNode {
    op: BoolOp,
}

impl Node for FeatureBooleanNode {
    fn op_name(&self) -> &'static str {
        "feature-boolean"
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
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
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
        let polygons = polygon_boolean(&a.polygons, &b.polygons, self.op);
        Ok(features_value(a.extent, polygons, vec![], vec![]))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"feature-boolean");
        h.update(match self.op {
            BoolOp::Union => b"u",
            BoolOp::Intersection => b"i",
            BoolOp::Difference => b"d",
            BoolOp::SymmetricDifference => b"s",
        });
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
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let a = take_input_ref(fields, "a")?;
        let b = take_input_ref(fields, "b")?;
        let op = match read_optional_string(fields, "mode")?.as_deref() {
            None | Some("union") => BoolOp::Union,
            Some("intersection") => BoolOp::Intersection,
            Some("difference") => BoolOp::Difference,
            Some("symmetric-difference") | Some("xor") => BoolOp::SymmetricDifference,
            Some(other) => {
                return Err(FactoryError::BadField {
                    field: "mode".into(),
                    msg: format!(
                        "expected one of union/intersection/difference/symmetric-difference, got `{other}`"
                    ),
                });
            }
        };
        Ok(BuiltNode {
            node: Box::new(FeatureBooleanNode { op }),
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
                "op": {
                    "type": "string",
                    "enum": ["union", "intersection", "difference", "symmetric-difference"],
                    "default": "union",
                },
            },
            "required": ["a", "b"],
        })
    }
}

ezu_graph::submit_node!(FeatureBooleanFactory);
