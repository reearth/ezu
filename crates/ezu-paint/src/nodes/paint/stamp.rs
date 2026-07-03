//! `stamp` — `Features + (Raster|Sprite) -> Raster`. Place the input
//! image once at every point in `Features.points`, with optional
//! rotation / scale / per-point jitter. The image is sampled at its
//! native dimensions regardless of which kind it was wired in as.
//!
//! Lines and polygons in the features input are ignored. The output
//! has the canvas-padded dimensions, matching every other paint node.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use tiny_skia::{PixmapPaint, PixmapRef, Transform};
use xxhash_rust::xxh3::Xxh3;

use ezu_core::{seed::world_seed, WorldPos};

use crate::nodes::common::{
    canvas_into_raster, downcast_features, empty_raster, make_canvas, unwrap_raster_or_sprite,
    ACCEPTS_RASTER_OR_SPRITE,
};

const STAMP_SALT: u32 = 0x5354_4d50; // 'STMP'

/// Parse an optional raw MapLibre expression field, type-checked against
/// `expect`. Returns `(parsed, raw_json_text)` for a stable cache hash.
fn parse_expr_field(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    expect: &maplibre_expr::Type,
) -> Result<(Option<maplibre_expr::Expr>, Option<String>), FactoryError> {
    match fields.get(name) {
        Some(v) => {
            let expr = maplibre_expr::parse(v).map_err(|e| FactoryError::BadField {
                field: name.into(),
                msg: e.to_string(),
            })?;
            let expr = maplibre_expr::typecheck(&expr, Some(expect), false).map_err(|e| {
                FactoryError::BadField {
                    field: name.into(),
                    msg: e.to_string(),
                }
            })?;
            Ok((Some(expr), Some(v.to_string())))
        }
        None => Ok((None, None)),
    }
}

/// Evaluate a `Number` expression for a group, falling back to `fallback`
/// when the expression is absent or doesn't resolve to a number.
fn eval_number(
    expr: &Option<maplibre_expr::Expr>,
    ectx: &maplibre_expr::EvaluationContext,
    fallback: f32,
) -> f32 {
    match expr {
        Some(e) => match maplibre_expr::evaluate(e, ectx) {
            Ok(maplibre_expr::Value::Number(n)) => n as f32,
            _ => fallback,
        },
        None => fallback,
    }
}

struct StampNode {
    scale: In<f64>,
    rotation_deg: In<f64>,
    rotation_jitter_deg: In<f64>,
    scale_jitter: In<f64>,
    opacity: In<f64>,
    /// Optional data-driven scale / rotation / opacity: MapLibre number
    /// expressions evaluated per feature group. When set, each overrides its
    /// constant counterpart for the group's points.
    scale_expr: Option<maplibre_expr::Expr>,
    rotation_deg_expr: Option<maplibre_expr::Expr>,
    opacity_expr: Option<maplibre_expr::Expr>,
    /// Raw `*-expr` JSON text, for a stable hash.
    scale_expr_src: Option<String>,
    rotation_deg_expr_src: Option<String>,
    opacity_expr_src: Option<String>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for StampNode {
    fn op_name(&self) -> &'static str {
        "stamp"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
    }
    fn coord_space(&self) -> CoordSpace {
        CoordSpace::World
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let feats = downcast_features(
            inputs[0]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("features".into()))?,
        )?;
        let image_in = inputs[1]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("image".into()))?;
        let (image, _) = unwrap_raster_or_sprite(image_in, "image")?;
        if !feats.has_points() || image.width == 0 || image.height == 0 {
            return Ok(empty_raster(ctx));
        }

        // Constants, resolved once. Data-driven exprs (if present) override
        // these per feature group; whichever expr is absent uses the constant.
        let const_scale = (self.scale.get(ctx, inputs)? as f32).max(0.0);
        let const_rotation_deg = self.rotation_deg.get(ctx, inputs)? as f32;
        let rotation_jitter_deg = self.rotation_jitter_deg.get(ctx, inputs)? as f32;
        let scale_jitter = self.scale_jitter.get(ctx, inputs)? as f32;
        let const_opacity = (self.opacity.get(ctx, inputs)? as f32).clamp(0.0, 1.0);

        let img_ref = PixmapRef::from_bytes(&image.pixels, image.width, image.height)
            .ok_or_else(|| EvalError::Other("stamp: invalid image pixmap bytes".into()))?;
        let iw = image.width as f32;
        let ih = image.height as f32;

        let mut canvas = make_canvas(ctx)?;
        let pad = canvas.pad() as f32;
        let tile_w = canvas.tile_width() as f32;
        let tile_h = canvas.tile_height() as f32;
        let extent = feats.extent.max(1) as f32;
        let sx = tile_w / extent;
        let sy = tile_h / extent;

        // World coords for deterministic jitter (matches the line / dab
        // nodes' world-seeded approach: a feature point gets the same
        // jitter regardless of which tile it lands on — and regardless of
        // which group it belongs to, since the seed is keyed by world
        // position only).
        let axis_tiles = (1u64 << ctx.tile.z) as f64;
        let world_origin_x = ctx.tile.x as f64 / axis_tiles;
        let world_origin_y = ctx.tile.y as f64 / axis_tiles;
        let world_per_px = 1.0 / (axis_tiles * tile_w as f64);

        // Stamp every point in `points` at the given scale / rotation /
        // opacity. Jitter is keyed by world position, so it does not depend
        // on the group the point came from.
        let stamp_points = |pm: &mut tiny_skia::Pixmap,
                            points: &[(i32, i32)],
                            scale: f32,
                            rotation_deg: f32,
                            pix_paint: &PixmapPaint| {
            for &(x, y) in points {
                let px = x as f32 * sx + pad;
                let py = y as f32 * sy + pad;
                let wx = world_origin_x + (px as f64 - pad as f64) * world_per_px;
                let wy = world_origin_y + (py as f64 - pad as f64) * world_per_px;

                let (mut rot_off, mut scale_off) = (0.0_f32, 0.0_f32);
                if rotation_jitter_deg != 0.0 || scale_jitter != 0.0 {
                    let mut seed = world_seed(WorldPos::new(wx, wy), STAMP_SALT);
                    rot_off = (next_unit(&mut seed) - 0.5) * 2.0 * rotation_jitter_deg;
                    scale_off = (next_unit(&mut seed) - 0.5) * 2.0 * scale_jitter;
                }
                let s = (scale * (1.0 + scale_off)).max(0.0);
                if s <= 0.0 {
                    continue;
                }
                let t = Transform::from_translate(px, py)
                    .pre_rotate(rotation_deg + rot_off)
                    .pre_scale(s, s)
                    .pre_translate(-iw * 0.5, -ih * 0.5);
                pm.draw_pixmap(0, 0, img_ref, pix_paint, t, None);
            }
        };

        let pm = canvas.pixmap_mut();
        if self.scale_expr.is_some()
            || self.rotation_deg_expr.is_some()
            || self.opacity_expr.is_some()
        {
            // Data-driven: resolve scale / rotation / opacity per feature
            // group and stamp each group's own points.
            let z = ctx.tile.z;
            for group in &feats.groups {
                let ectx = crate::render::group_expr_context(group, z);
                let scale = eval_number(&self.scale_expr, &ectx, const_scale).max(0.0);
                let rotation_deg = eval_number(&self.rotation_deg_expr, &ectx, const_rotation_deg);
                let opacity = eval_number(&self.opacity_expr, &ectx, const_opacity).clamp(0.0, 1.0);
                let pix_paint = PixmapPaint {
                    opacity,
                    ..PixmapPaint::default()
                };
                stamp_points(pm, &group.points, scale, rotation_deg, &pix_paint);
            }
        } else {
            let pix_paint = PixmapPaint {
                opacity: const_opacity,
                ..PixmapPaint::default()
            };
            let points: Vec<(i32, i32)> = feats.points().collect();
            stamp_points(pm, &points, const_scale, const_rotation_deg, &pix_paint);
        }

        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"stamp");
        self.scale.param_hash(h);
        self.rotation_deg.param_hash(h);
        self.rotation_jitter_deg.param_hash(h);
        self.scale_jitter.param_hash(h);
        self.opacity.param_hash(h);
        for (tag, src) in [
            (b"scaleexpr".as_slice(), &self.scale_expr_src),
            (b"rotationdegexpr".as_slice(), &self.rotation_deg_expr_src),
            (b"opacityexpr".as_slice(), &self.opacity_expr_src),
        ] {
            if let Some(s) = src {
                h.update(tag);
                h.update(s.as_bytes());
            }
        }
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

/// Deterministic per-point random draw in `[0, 1)`. Matches the PCG-style
/// step used in `strokes.rs::next_unit` so jitter behavior is consistent
/// across paint nodes.
#[inline]
fn next_unit(state: &mut u64) -> f32 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let x = (*state >> 33) as u32;
    (x as f32) * (1.0 / (1u64 << 32) as f32)
}

pub(super) struct StampFactory;
impl NodeFactory for StampFactory {
    fn op_name(&self) -> &'static str {
        "stamp"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let image = take_input_ref(fields, "image")?;
        let mut r = InReader::new(fields, ctx, 2);
        let scale = r.number_or("scale", 1.0)?;
        let rotation_deg = r.number_or("rotation-deg", 0.0)?;
        let rotation_jitter_deg = r.number_or("rotation-jitter-deg", 0.0)?;
        let scale_jitter = r.number_or("scale-jitter", 0.0)?;
        let opacity = r.number_or("opacity", 1.0)?;
        let parts = r.finish();

        let (scale_expr, scale_expr_src) =
            parse_expr_field(fields, "scale-expr", &maplibre_expr::Type::Number)?;
        let (rotation_deg_expr, rotation_deg_expr_src) =
            parse_expr_field(fields, "rotation-deg-expr", &maplibre_expr::Type::Number)?;
        let (opacity_expr, opacity_expr_src) =
            parse_expr_field(fields, "opacity-expr", &maplibre_expr::Type::Number)?;

        let mut ports = vec![
            PortSpec {
                name: "features",
                accepts: &[PortKind::Features],
                optional: false,
            },
            PortSpec {
                name: "image",
                accepts: ACCEPTS_RASTER_OR_SPRITE,
                optional: false,
            },
        ];
        ports.extend(parts.ports);
        let mut connections = vec![
            Connection {
                port: "features".into(),
                src: features,
            },
            Connection {
                port: "image".into(),
                src: image,
            },
        ];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(StampNode {
                scale,
                rotation_deg,
                rotation_jitter_deg,
                scale_jitter,
                opacity,
                scale_expr,
                rotation_deg_expr,
                opacity_expr,
                scale_expr_src,
                rotation_deg_expr_src,
                opacity_expr_src,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Stamp a sprite at every input point. Lines and polygons are ignored. Jitter is world-deterministic — a given point gets the same jitter no matter which tile renders it.",
            "properties": {
                "features": schema_frag::node_ref(),
                "image": schema_frag::node_ref(),
                "scale": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0,
                           "description": "Uniform scale applied to the sprite. Default 1.0 (native size)." })),
                "scale-expr": {
                    "description": "A MapLibre number expression, evaluated per feature group; overrides the constant `scale`. A group whose expression doesn't resolve to a number falls back to `scale`.",
                },
                "rotation-deg": schema_frag::in_number(serde_json::json!({ "type": "number",
                                  "description": "Constant rotation around each point, in degrees clockwise." })),
                "rotation-deg-expr": {
                    "description": "A MapLibre number expression (degrees clockwise), evaluated per feature group; overrides the constant `rotation-deg`.",
                },
                "rotation-jitter-deg": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0,
                                         "description": "Per-point random rotation, ±value degrees." })),
                "scale-jitter": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0,
                                  "description": "Per-point random scale, ±value as a fraction of `scale` (0.2 = ±20%)." })),
                "opacity": schema_frag::unit_number(),
                "opacity-expr": {
                    "description": "A MapLibre number expression giving opacity, evaluated per feature group; overrides the constant `opacity`.",
                },
            },
            "required": ["features", "image"],
        })
    }
}

ezu_graph::submit_node!(StampFactory);
