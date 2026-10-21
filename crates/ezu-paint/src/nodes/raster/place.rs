//! `place` — `Raster|Sprite -> Raster`. Draw the source onto the
//! canvas **once**, with a fit mode (`none` / `cover` / `contain` /
//! `stretch`) plus optional rotation and opacity. The input is
//! sampled at its native dimensions regardless of which kind it
//! came in as, so feeding a canvas-sized `Raster` works too — it
//! just happens to fit 1:1 under `cover`/`contain`.
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

use ezu_core::text::Anchor;
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EnumTable, EvalCtx, EvalError,
    FactoryCtx, FactoryError, In, InEnum, InReader, Node, NodeFactory, PortKind, PortSpec,
    PortValue,
};
use serde_json::Value;
use tiny_skia::{PixmapPaint, Transform};
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    canvas_into_raster, make_canvas, read_xy, unwrap_raster_or_sprite, warn_renamed, FramedImage,
    RasterKind, ACCEPTS_RASTER_OR_SPRITE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fit {
    None,
    Cover,
    Contain,
    Stretch,
}

const FITS: EnumTable<Fit> = &[
    ("none", Fit::None),
    ("cover", Fit::Cover),
    ("contain", Fit::Contain),
    ("stretch", Fit::Stretch),
];

/// `place` named its edge anchors `top-center` and so on before it took
/// MapLibre's names, which `text` and `stamp` use; the old names still
/// work, with a warning.
const RENAMED_ANCHORS: &[(&str, &str)] = &[
    ("top-center", "top"),
    ("center-left", "left"),
    ("center-right", "right"),
    ("bottom-center", "bottom"),
];

/// MapLibre's nine names, then the old ones for the four edges.
const ANCHORS: EnumTable<Anchor> = &[
    ("top-left", Anchor::TopLeft),
    ("top", Anchor::Top),
    ("top-right", Anchor::TopRight),
    ("left", Anchor::Left),
    ("center", Anchor::Center),
    ("right", Anchor::Right),
    ("bottom-left", Anchor::BottomLeft),
    ("bottom", Anchor::Bottom),
    ("bottom-right", Anchor::BottomRight),
    ("top-center", Anchor::Top),
    ("center-left", Anchor::Left),
    ("center-right", Anchor::Right),
    ("bottom-center", Anchor::Bottom),
];

struct PlaceNode {
    fit: InEnum<Fit>,
    position_px: [f32; 2],
    anchor: InEnum<Anchor>,
    scale: In<f64>,
    rotation_deg: In<f64>,
    opacity: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for PlaceNode {
    fn op_name(&self) -> &'static str {
        "place"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
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
        let input = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let (src, kind) = unwrap_raster_or_sprite(input, "input")?;
        let mut canvas = make_canvas(ctx)?;
        let Some(framed) = FramedImage::new(&src) else {
            return Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))));
        };

        let pad = canvas.pad() as f32;
        let tile_w = canvas.tile_width() as f32;
        let tile_h = canvas.tile_height() as f32;
        let sw = src.width as f32;
        let sh = src.height as f32;

        let scale = (self.scale.get(ctx, inputs)? as f32).max(0.0);
        // A sprite's pixels are style pixels, so at a higher pixel ratio it
        // is drawn that much larger; a canvas-sized raster is already in
        // canvas pixels. `position-px` is style pixels either way.
        let scale = match kind {
            RasterKind::Sprite => ctx.px32(scale),
            RasterKind::Raster => scale,
        };
        let position_px = [ctx.px32(self.position_px[0]), ctx.px32(self.position_px[1])];
        let rotation_deg = self.rotation_deg.get(ctx, inputs)? as f32;
        let opacity = (self.opacity.get(ctx, inputs)? as f32).clamp(0.0, 1.0);

        // Build the transform that maps source-image space to padded
        // canvas space. All builders end with `pre_translate(-ax, -ay)`
        // so the source-image anchor point ends up at the chosen
        // canvas position before rotation.
        let fit = self.fit.get(ctx)?;
        let transform = match fit {
            Fit::None => {
                let (fx, fy) = self.anchor.get(ctx)?.fraction();
                let ax = sw * fx;
                let ay = sh * fy;
                Transform::from_translate(position_px[0] + pad, position_px[1] + pad)
                    .pre_rotate(rotation_deg)
                    .pre_scale(scale, scale)
                    .pre_translate(-ax, -ay)
            }
            Fit::Cover | Fit::Contain => {
                let s_uniform = if fit == Fit::Cover {
                    (tile_w / sw).max(tile_h / sh)
                } else {
                    (tile_w / sw).min(tile_h / sh)
                };
                let cx = tile_w * 0.5 + position_px[0] + pad;
                let cy = tile_h * 0.5 + position_px[1] + pad;
                Transform::from_translate(cx, cy)
                    .pre_rotate(rotation_deg)
                    .pre_scale(s_uniform, s_uniform)
                    .pre_translate(-sw * 0.5, -sh * 0.5)
            }
            Fit::Stretch => {
                let sx = tile_w / sw;
                let sy = tile_h / sh;
                let cx = tile_w * 0.5 + position_px[0] + pad;
                let cy = tile_h * 0.5 + position_px[1] + pad;
                Transform::from_translate(cx, cy)
                    .pre_rotate(rotation_deg)
                    .pre_scale(sx, sy)
                    .pre_translate(-sw * 0.5, -sh * 0.5)
            }
        };

        let paint = PixmapPaint {
            opacity,
            ..PixmapPaint::default()
        };
        framed.draw(&mut canvas.pixmap_mut().as_mut(), &paint, transform);
        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"place");
        self.fit.param_hash(h);
        h.update(&self.position_px[0].to_le_bytes());
        h.update(&self.position_px[1].to_le_bytes());
        self.anchor.param_hash(h);
        self.scale.param_hash(h);
        self.rotation_deg.param_hash(h);
        self.opacity.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        let position_px = read_xy(fields, "position-px", ctx, [0.0, 0.0])?;
        let mut r = InReader::new(fields, ctx, 1);
        let fit = r.enum_or("fit", Fit::None, FITS)?;
        let anchor = r.enum_or("anchor", Anchor::TopLeft, ANCHORS)?;
        warn_renamed(fields, ctx, "anchor", RENAMED_ANCHORS);
        let scale = r.number_or("scale", 1.0)?;
        let rotation_deg = r.number_or("rotation-deg", 0.0)?;
        let opacity = r.number_or("opacity", 1.0)?;
        let parts = r.finish();

        let mut ports = vec![PortSpec {
            name: "input",
            accepts: ACCEPTS_RASTER_OR_SPRITE,
            optional: false,
        }];
        ports.extend(parts.ports);
        let mut connections = vec![Connection {
            port: "input".into(),
            src: input,
        }];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(PlaceNode {
                fit,
                position_px,
                anchor,
                scale,
                rotation_deg,
                opacity,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Draw the source raster onto the canvas exactly once. `fit:none` (default) uses position/anchor/scale; `cover`/`contain`/`stretch` derive the destination rect from the canvas (and treat `position-px` as an offset from canvas center). Companion to `tiling` (repeats) and `stamp` (per-point).",
            "properties": {
                "input": schema_frag::node_ref(),
                "fit": schema_frag::in_enum(serde_json::json!({ "type": "string",
                         "enum": ["none", "cover", "contain", "stretch"],
                         "default": "none" })),
                "position-px": { "type": "array",
                                 "items": { "type": "number" },
                                 "minItems": 2, "maxItems": 2,
                                 "description": "fit=none: point in pixels from the tile's top-left where the image's `anchor` point lands. Other fits: offset from canvas center." },
                "anchor": schema_frag::in_enum(serde_json::json!({ "type": "string",
                            "enum": ezu_graph::enum_names(ANCHORS),
                            "default": "top-left",
                            "description": "Which point of the image lands on `position-px`, by MapLibre's anchor names, as `text` and `stamp` take them. `top-center`, `center-left`, `center-right` and `bottom-center` are deprecated names for `top`, `left`, `right` and `bottom`. Only used when fit=none." })),
                "scale": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0,
                           "description": "Uniform scale for fit=none. Ignored for other fits." })),
                "rotation-deg": schema_frag::in_number(serde_json::json!({ "type": "number",
                                  "description": "Rotation (degrees clockwise) around the placement anchor / canvas center." })),
                "opacity": schema_frag::unit_number(),
            },
            "required": ["input"],
        })
    }
}

ezu_graph::submit_node!(PlaceFactory);
