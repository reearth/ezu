//! `line-stamp` — `Features + (Raster|Sprite) -> Raster`. Repeat a sprite
//! *along* each polyline, oriented to the local tangent — the primitive
//! behind MapLibre's `line-pattern`.
//!
//! Where `stamp` places an image at each point and `tiling` fills the
//! canvas, `line-stamp` walks each `Features.lines` polyline by arc length
//! and pastes the image at a fixed spacing, rotated to follow the line.
//! With `width-px` the image is scaled so its height matches the stroke
//! width (pattern height → line width, the MapLibre convention); spacing
//! then defaults to the scaled image width so the pattern tiles seamlessly.
//!
//! Points and polygons in the features input are ignored. The walk carries
//! its leftover distance across segment joins, so spacing is continuous
//! along the whole polyline (independent per tile).

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use tiny_skia::{PixmapPaint, PixmapRef, Transform};
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    canvas_into_raster, downcast_features, empty_raster, make_canvas, unwrap_raster_or_sprite,
    ACCEPTS_RASTER_OR_SPRITE,
};

struct LineStampNode {
    /// Stroke width in px. When > 0 the image is scaled so its height
    /// matches this (its native height otherwise); 0 = native size.
    width_px: In<f64>,
    /// Advance between stamps in px. 0 = auto (the scaled image width, for
    /// seamless tiling).
    spacing_px: In<f64>,
    scale: In<f64>,
    opacity: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for LineStampNode {
    fn op_name(&self) -> &'static str {
        "line-stamp"
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
        if feats.lines.is_empty() || image.width == 0 || image.height == 0 {
            return Ok(empty_raster(ctx));
        }

        let width_px = self.width_px.get(ctx, inputs)? as f32;
        let spacing_in = self.spacing_px.get(ctx, inputs)? as f32;
        let base_scale = (self.scale.get(ctx, inputs)? as f32).max(0.0);
        let opacity = (self.opacity.get(ctx, inputs)? as f32).clamp(0.0, 1.0);

        let iw = image.width as f32;
        let ih = image.height as f32;
        // `width-px` fits the image height to the stroke width.
        let fit = if width_px > 0.0 { width_px / ih } else { 1.0 };
        let scale = base_scale * fit;
        if scale <= 0.0 {
            return Ok(empty_raster(ctx));
        }
        let spacing = if spacing_in > 0.0 {
            spacing_in
        } else {
            (iw * scale).max(1.0)
        };

        let img_ref = PixmapRef::from_bytes(&image.pixels, image.width, image.height)
            .ok_or_else(|| EvalError::Other("line-stamp: invalid image pixmap bytes".into()))?;
        let pix_paint = PixmapPaint {
            opacity,
            ..PixmapPaint::default()
        };

        let mut canvas = make_canvas(ctx)?;
        let pad = canvas.pad() as f32;
        let tile_w = canvas.tile_width() as f32;
        let tile_h = canvas.tile_height() as f32;
        let extent = feats.extent.max(1) as f32;
        let sx = tile_w / extent;
        let sy = tile_h / extent;
        let pm = canvas.pixmap_mut();

        for line in &feats.lines {
            if line.len() < 2 {
                continue;
            }
            let pts: Vec<(f32, f32)> = line
                .iter()
                .map(|&(x, y)| (x as f32 * sx + pad, y as f32 * sy + pad))
                .collect();
            // Distance until the next stamp; start half a spacing in so the
            // first stamp sits inside the line rather than at the vertex.
            let mut next = spacing * 0.5;
            for w in pts.windows(2) {
                let (x0, y0) = w[0];
                let (x1, y1) = w[1];
                let dx = x1 - x0;
                let dy = y1 - y0;
                let seg = (dx * dx + dy * dy).sqrt();
                if seg < 1e-6 {
                    continue;
                }
                let angle = dy.atan2(dx).to_degrees();
                while next <= seg {
                    let f = next / seg;
                    let px = x0 + dx * f;
                    let py = y0 + dy * f;
                    let t = Transform::from_translate(px, py)
                        .pre_rotate(angle)
                        .pre_scale(scale, scale)
                        .pre_translate(-iw * 0.5, -ih * 0.5);
                    pm.draw_pixmap(0, 0, img_ref, &pix_paint, t, None);
                    next += spacing;
                }
                next -= seg;
            }
        }

        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"line-stamp");
        self.width_px.param_hash(h);
        self.spacing_px.param_hash(h);
        self.scale.param_hash(h);
        self.opacity.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct LineStampFactory;
impl NodeFactory for LineStampFactory {
    fn op_name(&self) -> &'static str {
        "line-stamp"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let image = take_input_ref(fields, "image")?;
        let mut r = InReader::new(fields, ctx, 2);
        let width_px = r.number_or("width-px", 0.0)?;
        let spacing_px = r.number_or("spacing-px", 0.0)?;
        let scale = r.number_or("scale", 1.0)?;
        let opacity = r.number_or("opacity", 1.0)?;
        let parts = r.finish();

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
            node: Box::new(LineStampNode {
                width_px,
                spacing_px,
                scale,
                opacity,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Repeat a sprite along each polyline, rotated to the line's tangent (MapLibre `line-pattern`). Points/polygons ignored. `width-px` scales the image height to the stroke width; `spacing-px` sets the advance (0 = the scaled image width, seamless).",
            "properties": {
                "features": schema_frag::node_ref(),
                "image": schema_frag::node_ref(),
                "width-px": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0,
                              "description": "Stroke width; the image height is fit to it. 0 = native height." })),
                "spacing-px": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0,
                                "description": "Advance between stamps. 0 = the scaled image width (seamless tiling)." })),
                "scale": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0,
                           "description": "Extra uniform scale on top of the width fit. Default 1.0." })),
                "opacity": schema_frag::unit_number(),
            },
            "required": ["features", "image"],
        })
    }
}

ezu_graph::submit_node!(LineStampFactory);
