//! `stamp` — `Features + Raster -> Raster`. Place the input `Raster`
//! (a sprite) once at every point in `Features.points`, with optional
//! rotation / scale / per-point jitter.
//!
//! Lines and polygons in the features input are ignored. The output
//! has the canvas-padded dimensions, matching every other paint node.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use tiny_skia::{PixmapPaint, PixmapRef, Transform};
use xxhash_rust::xxh3::Xxh3;

use ezu_core::{seed::world_seed, WorldPos};

use crate::nodes::common::{
    canvas_into_raster, downcast_features, empty_raster, make_canvas, read_number_or,
};

const STAMP_SALT: u32 = 0x5354_4d50; // 'STMP'

struct StampNode {
    scale: f32,
    rotation_deg: f32,
    rotation_jitter_deg: f32,
    scale_jitter: f32,
    opacity: f32,
}

impl Node for StampNode {
    fn op_name(&self) -> &'static str {
        "stamp"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[
            PortSpec {
                name: "features",
                kind: PortKind::Features,
                optional: false,
            },
            PortSpec {
                name: "image",
                kind: PortKind::Sprite,
                optional: false,
            },
        ];
        SPECS
    }
    fn output(&self) -> PortKind {
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
        let image = inputs[1]
            .as_ref()
            .and_then(PortValue::as_sprite)
            .ok_or_else(|| EvalError::MissingInput("image".into()))?
            .clone();
        if feats.points.is_empty() || image.width == 0 || image.height == 0 {
            return Ok(empty_raster(ctx));
        }

        let img_ref = PixmapRef::from_bytes(&image.pixels, image.width, image.height)
            .ok_or_else(|| EvalError::Other("stamp: invalid image pixmap bytes".into()))?;
        let iw = image.width as f32;
        let ih = image.height as f32;
        let pix_paint = PixmapPaint {
            opacity: self.opacity,
            ..PixmapPaint::default()
        };

        let mut canvas = make_canvas(ctx)?;
        let pad = canvas.pad() as f32;
        let tile_w = canvas.tile_width() as f32;
        let tile_h = canvas.tile_height() as f32;
        let extent = feats.extent.max(1) as f32;
        let sx = tile_w / extent;
        let sy = tile_h / extent;

        // World coords for deterministic jitter (matches the line / dab
        // nodes' world-seeded approach: a feature point gets the same
        // jitter regardless of which tile it lands on).
        let axis_tiles = (1u64 << ctx.tile.z) as f64;
        let world_origin_x = ctx.tile.x as f64 / axis_tiles;
        let world_origin_y = ctx.tile.y as f64 / axis_tiles;
        let world_per_px = 1.0 / (axis_tiles * tile_w as f64);

        let pm = canvas.pixmap_mut();
        for &(x, y) in &feats.points {
            let px = x as f32 * sx + pad;
            let py = y as f32 * sy + pad;
            let wx = world_origin_x + (px as f64 - pad as f64) * world_per_px;
            let wy = world_origin_y + (py as f64 - pad as f64) * world_per_px;

            let (mut rot_off, mut scale_off) = (0.0_f32, 0.0_f32);
            if self.rotation_jitter_deg != 0.0 || self.scale_jitter != 0.0 {
                let mut seed = world_seed(WorldPos::new(wx, wy), STAMP_SALT);
                rot_off = (next_unit(&mut seed) - 0.5) * 2.0 * self.rotation_jitter_deg;
                scale_off = (next_unit(&mut seed) - 0.5) * 2.0 * self.scale_jitter;
            }
            let s = (self.scale * (1.0 + scale_off)).max(0.0);
            if s <= 0.0 {
                continue;
            }
            let t = Transform::from_translate(px, py)
                .pre_rotate(self.rotation_deg + rot_off)
                .pre_scale(s, s)
                .pre_translate(-iw * 0.5, -ih * 0.5);
            pm.draw_pixmap(0, 0, img_ref, &pix_paint, t, None);
        }

        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"stamp");
        for v in [
            self.scale,
            self.rotation_deg,
            self.rotation_jitter_deg,
            self.scale_jitter,
            self.opacity,
        ] {
            h.update(&v.to_le_bytes());
        }
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
        let scale = read_number_or(fields, "scale", ctx, 1.0)? as f32;
        let rotation_deg = read_number_or(fields, "rotation-deg", ctx, 0.0)? as f32;
        let rotation_jitter_deg = read_number_or(fields, "rotation-jitter-deg", ctx, 0.0)? as f32;
        let scale_jitter = read_number_or(fields, "scale-jitter", ctx, 0.0)? as f32;
        let opacity = read_number_or(fields, "opacity", ctx, 1.0)?.clamp(0.0, 1.0) as f32;
        Ok(BuiltNode {
            node: Box::new(StampNode {
                scale: scale.max(0.0),
                rotation_deg,
                rotation_jitter_deg,
                scale_jitter,
                opacity,
            }),
            connections: vec![
                Connection {
                    port: "features".into(),
                    src: features,
                },
                Connection {
                    port: "image".into(),
                    src: image,
                },
            ],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Stamp a sprite at every input point. Lines and polygons are ignored. Jitter is world-deterministic — a given point gets the same jitter no matter which tile renders it.",
            "properties": {
                "features": schema_frag::node_ref(),
                "image": schema_frag::node_ref(),
                "scale": { "type": "number", "minimum": 0.0,
                           "description": "Uniform scale applied to the sprite. Default 1.0 (native size)." },
                "rotation-deg": { "type": "number",
                                  "description": "Constant rotation around each point, in degrees clockwise." },
                "rotation-jitter-deg": { "type": "number", "minimum": 0.0,
                                         "description": "Per-point random rotation, ±value degrees." },
                "scale-jitter": { "type": "number", "minimum": 0.0,
                                  "description": "Per-point random scale, ±value as a fraction of `scale` (0.2 = ±20%)." },
                "opacity": schema_frag::unit_number(),
            },
            "required": ["features", "image"],
        })
    }
}

ezu_graph::submit_node!(StampFactory);
