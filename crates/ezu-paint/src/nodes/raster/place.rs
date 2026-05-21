//! `place` — `Raster -> Raster`. Draw the source raster onto the
//! canvas **once**, with a fit mode (`none` / `cover` / `contain` /
//! `stretch`) plus optional rotation and opacity.
//!
//! Companion to `tiling` (which repeats the source) and `stamp` (which
//! pastes the source per-point). Use `place` for hero backgrounds,
//! logos, watermarks, or any "one image on the tile" composition.
//!
//! - `fit: "none"` (default) uses `position-px`, `anchor`, `scale`,
//!   and `rotation-deg` for manual placement.
//! - `fit: "cover" | "contain" | "stretch"` ignores `scale` /
//!   `anchor` and derives the destination rect from the canvas size;
//!   `position-px` acts as an offset from the canvas center; rotation
//!   is applied around that point.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError,
    FactoryCtx, FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use tiny_skia::{PixmapPaint, PixmapRef, Transform};
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    canvas_into_raster, make_canvas, read_number_or, read_optional_string, read_xy,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fit {
    None,
    Cover,
    Contain,
    Stretch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor9 {
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl Anchor9 {
    /// Return the (fx, fy) in `[0, 1]` × `[0, 1]` of the anchor point
    /// inside the source image. Multiplying by `(src.w, src.h)` gives
    /// the pixel coordinate of the anchor.
    fn fractions(self) -> (f32, f32) {
        match self {
            Self::TopLeft => (0.0, 0.0),
            Self::TopCenter => (0.5, 0.0),
            Self::TopRight => (1.0, 0.0),
            Self::CenterLeft => (0.0, 0.5),
            Self::Center => (0.5, 0.5),
            Self::CenterRight => (1.0, 0.5),
            Self::BottomLeft => (0.0, 1.0),
            Self::BottomCenter => (0.5, 1.0),
            Self::BottomRight => (1.0, 1.0),
        }
    }
}

struct PlaceNode {
    fit: Fit,
    position_px: [f32; 2],
    anchor: Anchor9,
    scale: f32,
    rotation_deg: f32,
    opacity: f32,
}

impl Node for PlaceNode {
    fn op_name(&self) -> &'static str {
        "place"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "input",
            kind: PortKind::Raster,
            optional: false,
        }];
        SPECS
    }
    fn output(&self) -> PortKind {
        PortKind::Raster
    }
    fn coord_space(&self) -> CoordSpace {
        CoordSpace::Tile
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let src = inputs[0]
            .as_ref()
            .and_then(PortValue::as_raster)
            .ok_or_else(|| EvalError::MissingInput("input".into()))?
            .clone();
        let mut canvas = make_canvas(ctx);
        if src.width == 0 || src.height == 0 {
            return Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))));
        }
        let img_ref = PixmapRef::from_bytes(&src.pixels, src.width, src.height)
            .ok_or_else(|| EvalError::Other("place: invalid image pixmap bytes".into()))?;

        let pad = canvas.pad() as f32;
        let tile_w = canvas.tile_width() as f32;
        let tile_h = canvas.tile_height() as f32;
        let sw = src.width as f32;
        let sh = src.height as f32;

        // Build the transform that maps source-image space to padded
        // canvas space. All builders end with `pre_translate(-ax, -ay)`
        // so the source-image anchor point ends up at the chosen
        // canvas position before rotation.
        let transform = match self.fit {
            Fit::None => {
                let (fx, fy) = self.anchor.fractions();
                let ax = sw * fx;
                let ay = sh * fy;
                Transform::from_translate(self.position_px[0] + pad, self.position_px[1] + pad)
                    .pre_rotate(self.rotation_deg)
                    .pre_scale(self.scale, self.scale)
                    .pre_translate(-ax, -ay)
            }
            Fit::Cover | Fit::Contain => {
                let s_uniform = if self.fit == Fit::Cover {
                    (tile_w / sw).max(tile_h / sh)
                } else {
                    (tile_w / sw).min(tile_h / sh)
                };
                let cx = tile_w * 0.5 + self.position_px[0] + pad;
                let cy = tile_h * 0.5 + self.position_px[1] + pad;
                Transform::from_translate(cx, cy)
                    .pre_rotate(self.rotation_deg)
                    .pre_scale(s_uniform, s_uniform)
                    .pre_translate(-sw * 0.5, -sh * 0.5)
            }
            Fit::Stretch => {
                let sx = tile_w / sw;
                let sy = tile_h / sh;
                let cx = tile_w * 0.5 + self.position_px[0] + pad;
                let cy = tile_h * 0.5 + self.position_px[1] + pad;
                Transform::from_translate(cx, cy)
                    .pre_rotate(self.rotation_deg)
                    .pre_scale(sx, sy)
                    .pre_translate(-sw * 0.5, -sh * 0.5)
            }
        };

        let paint = PixmapPaint {
            opacity: self.opacity,
            ..PixmapPaint::default()
        };
        canvas
            .pixmap_mut()
            .draw_pixmap(0, 0, img_ref, &paint, transform, None);
        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"place");
        match self.fit {
            Fit::None => h.update(&[0]),
            Fit::Cover => h.update(&[1]),
            Fit::Contain => h.update(&[2]),
            Fit::Stretch => h.update(&[3]),
        }
        h.update(&self.position_px[0].to_le_bytes());
        h.update(&self.position_px[1].to_le_bytes());
        h.update(&(self.anchor as u8).to_le_bytes());
        h.update(&self.scale.to_le_bytes());
        h.update(&self.rotation_deg.to_le_bytes());
        h.update(&self.opacity.to_le_bytes());
    }
}

pub(super) struct PlaceFactory;
impl NodeFactory for PlaceFactory {
    fn op_name(&self) -> &'static str {
        "place"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let fit = match read_optional_string(fields, "fit")?.as_deref() {
            None | Some("none") => Fit::None,
            Some("cover") => Fit::Cover,
            Some("contain") => Fit::Contain,
            Some("stretch") => Fit::Stretch,
            Some(other) => {
                return Err(FactoryError::BadField {
                    field: "fit".into(),
                    msg: format!("expected `none`/`cover`/`contain`/`stretch`, got `{other}`"),
                });
            }
        };
        let position_px = read_xy(fields, "position-px", ctx, [0.0, 0.0])?;
        let anchor = match read_optional_string(fields, "anchor")?.as_deref() {
            None | Some("top-left") => Anchor9::TopLeft,
            Some("top-center") => Anchor9::TopCenter,
            Some("top-right") => Anchor9::TopRight,
            Some("center-left") => Anchor9::CenterLeft,
            Some("center") => Anchor9::Center,
            Some("center-right") => Anchor9::CenterRight,
            Some("bottom-left") => Anchor9::BottomLeft,
            Some("bottom-center") => Anchor9::BottomCenter,
            Some("bottom-right") => Anchor9::BottomRight,
            Some(other) => {
                return Err(FactoryError::BadField {
                    field: "anchor".into(),
                    msg: format!("unknown anchor `{other}`"),
                });
            }
        };
        let scale = read_number_or(fields, "scale", ctx, 1.0)? as f32;
        let rotation_deg = read_number_or(fields, "rotation-deg", ctx, 0.0)? as f32;
        let opacity = read_number_or(fields, "opacity", ctx, 1.0)?.clamp(0.0, 1.0) as f32;
        Ok(BuiltNode {
            node: Box::new(PlaceNode {
                fit,
                position_px,
                anchor,
                scale: scale.max(0.0),
                rotation_deg,
                opacity,
            }),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Draw the source raster onto the canvas exactly once. `fit:none` (default) uses position/anchor/scale; `cover`/`contain`/`stretch` derive the destination rect from the canvas (and treat `position-px` as an offset from canvas center). Companion to `tiling` (repeats) and `stamp` (per-point).",
            "properties": {
                "input": schema_frag::node_ref(),
                "fit": { "type": "string",
                         "enum": ["none", "cover", "contain", "stretch"],
                         "default": "none" },
                "position-px": { "type": "array",
                                 "items": { "type": "number" },
                                 "minItems": 2, "maxItems": 2,
                                 "description": "fit=none: canvas pixel where the image's `anchor` point lands. Other fits: offset from canvas center." },
                "anchor": { "type": "string",
                            "enum": ["top-left", "top-center", "top-right",
                                     "center-left", "center", "center-right",
                                     "bottom-left", "bottom-center", "bottom-right"],
                            "default": "top-left",
                            "description": "Only used when fit=none." },
                "scale": { "type": "number", "minimum": 0.0,
                           "description": "Uniform scale for fit=none. Ignored for other fits." },
                "rotation-deg": { "type": "number",
                                  "description": "Rotation (degrees clockwise) around the placement anchor / canvas center." },
                "opacity": schema_frag::unit_number(),
            },
            "required": ["input"],
        })
    }
}

ezu_graph::submit_node!(PlaceFactory);
