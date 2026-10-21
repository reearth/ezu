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
//! Width, opacity and the pattern itself can each be data-driven, resolved
//! per feature group the way `stroke` and `stamp` resolve theirs.
//!
//! Points and polygons in the features input are ignored. The walk carries
//! its leftover distance across segment joins, so spacing is continuous
//! along the whole polyline (independent per tile).

use std::collections::HashMap;
use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, Asset, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError,
    FactoryCtx, FactoryError, In, InReader, InfluenceCtx, Node, NodeFactory, PortKind, PortSpec,
    PortValue,
};
use serde_json::Value;
use tiny_skia::{PixmapPaint, Transform};
use xxhash_rust::xxh3::Xxh3;

use super::stamp::{eval_icon_name, eval_number, eval_px, parse_expr_field, read_icon_name_expr};
use crate::nodes::common::{
    canvas_into_raster, downcast_features, empty_raster, make_canvas, unwrap_raster_or_sprite,
    FramedImage, ACCEPTS_RASTER_OR_SPRITE,
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
    /// Optional data-driven width / opacity: MapLibre number expressions
    /// evaluated per feature group. When set, each overrides its constant
    /// counterpart for the group's lines.
    width_expr: Option<maplibre_expr::Expr>,
    opacity_expr: Option<maplibre_expr::Expr>,
    /// Raw `*-expr` JSON text, for a stable hash.
    width_expr_src: Option<String>,
    opacity_expr_src: Option<String>,
    /// Data-driven pattern: instead of the `image` port, the sprite atlas key
    /// plus a MapLibre expression naming each feature's pattern icon, cropped
    /// per group from the bound sheet. Absent → the node repeats its single
    /// `image` input.
    pattern_sprite: Option<String>,
    pattern_name_expr: Option<maplibre_expr::Expr>,
    pattern_name_expr_src: Option<String>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

/// The per-eval constants a group's lines are stamped with when no
/// expression overrides them.
struct Walk {
    pad: f32,
    sx: f32,
    sy: f32,
    spacing_in: f32,
    base_scale: f32,
    /// The scale a sprite is drawn at when no width fits it: its pixels are
    /// style px, so one device px per style px.
    native: f32,
}

impl Walk {
    /// Stamp `img` along every polyline in `lines`, its height fit to
    /// `width_px` (native when 0) and drawn at `opacity`.
    fn stamp_lines(
        &self,
        pm: &mut tiny_skia::PixmapMut<'_>,
        lines: &[Vec<(i32, i32)>],
        img: &FramedImage,
        width_px: f32,
        opacity: f32,
    ) {
        let (iw, ih) = img.size();
        // `width-px` fits the image height to the stroke width.
        let fit = if width_px > 0.0 {
            width_px / ih
        } else {
            self.native
        };
        let scale = self.base_scale * fit;
        if scale <= 0.0 {
            return;
        }
        let spacing = if self.spacing_in > 0.0 {
            self.spacing_in
        } else {
            (iw * scale).max(1.0)
        };
        let pix_paint = PixmapPaint {
            opacity,
            ..PixmapPaint::default()
        };
        for line in lines {
            if line.len() < 2 {
                continue;
            }
            let pts: Vec<(f32, f32)> = line
                .iter()
                .map(|&(x, y)| (x as f32 * self.sx + self.pad, y as f32 * self.sy + self.pad))
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
                let angle = libm::atan2f(dy, dx).to_degrees();
                while next <= seg {
                    let f = next / seg;
                    let px = x0 + dx * f;
                    let py = y0 + dy * f;
                    let t = Transform::from_translate(px, py)
                        .pre_rotate(angle)
                        .pre_scale(scale, scale)
                        .pre_translate(-iw * 0.5, -ih * 0.5);
                    img.draw(pm, &pix_paint, t);
                    next += spacing;
                }
                next -= seg;
            }
        }
    }
}

impl Node for LineStampNode {
    fn op_name(&self) -> &'static str {
        "line-stamp"
    }

    /// Nothing upstream of this op may be dropped.
    ///
    /// The sprite's own size is only known once the image is loaded.
    fn influence_pad(&self, _ctx: &InfluenceCtx<'_>) -> u32 {
        InfluenceCtx::UNBOUNDED
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
        // The single pattern, when one is wired rather than named per group.
        let image = match &self.pattern_name_expr {
            Some(_) => None,
            None => {
                let image_in = inputs[1]
                    .as_ref()
                    .ok_or_else(|| EvalError::MissingInput("image".into()))?;
                Some(unwrap_raster_or_sprite(image_in, "image")?.0)
            }
        };
        if !feats.has_lines() {
            return Ok(empty_raster(ctx));
        }
        if let Some(image) = &image {
            if image.width == 0 || image.height == 0 {
                return Ok(empty_raster(ctx));
            }
        }

        // Constants, resolved once. Data-driven exprs (if present) override
        // these per feature group; whichever expr is absent uses the constant.
        // Width and spacing are style px, drawn in device px; a sprite's
        // pixels are style px too, so at its native size it is scaled by
        // the ratio.
        let const_width_px = ctx.px32(self.width_px.get(ctx, inputs)? as f32);
        let spacing_in = ctx.px32(self.spacing_px.get(ctx, inputs)? as f32);
        let base_scale = (self.scale.get(ctx, inputs)? as f32).max(0.0);
        let const_opacity = (self.opacity.get(ctx, inputs)? as f32).clamp(0.0, 1.0);

        let mut canvas = make_canvas(ctx)?;
        let extent = feats.extent.max(1) as f32;
        let walk = Walk {
            pad: canvas.pad() as f32,
            sx: canvas.tile_width() as f32 / extent,
            sy: canvas.tile_height() as f32 / extent,
            spacing_in,
            base_scale,
            native: ctx.px32(1.0),
        };
        let z = ctx.tile.z;
        let has_paint_expr = self.width_expr.is_some() || self.opacity_expr.is_some();
        let mut pm = canvas.pixmap_mut().as_mut();

        if let Some(name_expr) = &self.pattern_name_expr {
            // Data-driven pattern: crop each group's icon from the bound
            // sprite sheet, caching the crops by name.
            let key = self.pattern_sprite.as_deref().ok_or_else(|| {
                EvalError::Other("line-stamp: pattern by name without a sprite".into())
            })?;
            let Asset::Sprite(sheet) = ctx.assets.load(key)? else {
                return Err(EvalError::Other(format!(
                    "asset `{key}` is not a sprite sheet"
                )));
            };
            let mut crops: HashMap<String, Option<FramedImage>> = HashMap::new();
            for group in &feats.groups {
                if group.lines.is_empty() {
                    continue;
                }
                let ectx = crate::render::group_expr_context(group, z);
                let Some(name) = eval_icon_name(name_expr, &ectx) else {
                    continue;
                };
                let Some(img) = crops
                    .entry(name.clone())
                    .or_insert_with(|| sheet.crop(&name).as_ref().and_then(FramedImage::new))
                else {
                    continue;
                };
                let width_px = eval_number(&self.width_expr, &ectx, const_width_px);
                let opacity = eval_number(&self.opacity_expr, &ectx, const_opacity).clamp(0.0, 1.0);
                walk.stamp_lines(&mut pm, &group.lines, img, width_px, opacity);
            }
        } else {
            let image = image.expect("an image is wired when no pattern is named");
            let Some(img) = FramedImage::new(&image) else {
                return Ok(empty_raster(ctx));
            };
            if has_paint_expr {
                for group in &feats.groups {
                    if group.lines.is_empty() {
                        continue;
                    }
                    let ectx = crate::render::group_expr_context(group, z);
                    let width_px = eval_px(&self.width_expr, &ectx, const_width_px, ctx);
                    let opacity =
                        eval_number(&self.opacity_expr, &ectx, const_opacity).clamp(0.0, 1.0);
                    walk.stamp_lines(&mut pm, &group.lines, &img, width_px, opacity);
                }
            } else {
                // Every group stamps the same way, so walk them in order
                // without building an expression context for each.
                for group in &feats.groups {
                    walk.stamp_lines(&mut pm, &group.lines, &img, const_width_px, const_opacity);
                }
            }
        }

        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn asset_inputs(&self) -> Vec<String> {
        // With a pattern named per group the sprite sheet is bound here
        // (wasm hosts pre-bind).
        self.pattern_sprite.iter().cloned().collect()
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"line-stamp");
        self.width_px.param_hash(h);
        self.spacing_px.param_hash(h);
        self.scale.param_hash(h);
        self.opacity.param_hash(h);
        for (tag, src) in [
            (b"widthexpr".as_slice(), &self.width_expr_src),
            (b"opacityexpr".as_slice(), &self.opacity_expr_src),
            (b"nameexpr".as_slice(), &self.pattern_name_expr_src),
        ] {
            if let Some(s) = src {
                h.update(tag);
                h.update(s.as_bytes());
            }
        }
        if let Some(sprite) = &self.pattern_sprite {
            h.update(b"sprite");
            h.update(sprite.as_bytes());
        }
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
        let (pattern_sprite, pattern_name_expr, pattern_name_expr_src) =
            read_icon_name_expr(fields, ctx)?;
        // The single-image input, present only when no pattern is named.
        let image = match pattern_name_expr {
            Some(_) => None,
            None => Some(take_input_ref(fields, "image")?),
        };
        let mut r = InReader::new(fields, ctx, if image.is_some() { 2 } else { 1 });
        let width_px = r.number_or("width-px", 0.0)?;
        let spacing_px = r.number_or("spacing-px", 0.0)?;
        let scale = r.number_or("scale", 1.0)?;
        let opacity = r.number_or("opacity", 1.0)?;
        let parts = r.finish();

        let (width_expr, width_expr_src) =
            parse_expr_field(fields, "width-expr", &maplibre_expr::Type::Number)?;
        let (opacity_expr, opacity_expr_src) =
            parse_expr_field(fields, "opacity-expr", &maplibre_expr::Type::Number)?;

        let mut ports = vec![PortSpec {
            name: "features",
            accepts: &[PortKind::Features],
            optional: false,
        }];
        let mut connections = vec![Connection {
            port: "features".into(),
            src: features,
        }];
        if let Some(image) = image {
            ports.push(PortSpec {
                name: "image",
                accepts: ACCEPTS_RASTER_OR_SPRITE,
                optional: false,
            });
            connections.push(Connection {
                port: "image".into(),
                src: image,
            });
        }
        ports.extend(parts.ports);
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(LineStampNode {
                width_px,
                spacing_px,
                scale,
                opacity,
                width_expr,
                opacity_expr,
                width_expr_src,
                opacity_expr_src,
                pattern_sprite,
                pattern_name_expr,
                pattern_name_expr_src,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Repeat a sprite along each polyline, rotated to the line's tangent (MapLibre `line-pattern`). Points/polygons ignored. `width-px` scales the image height to the stroke width; `spacing-px` sets the advance (0 = the scaled image width, seamless). Provide either an `image` input (one pattern for every line) or, for a data-driven `line-pattern`, a `sprite` atlas ref plus a `name-expr` that names each feature's pattern.",
            "properties": {
                "features": schema_frag::node_ref(),
                "image": schema_frag::node_ref(),
                "sprite": schema_frag::asset_ref(),
                "name-expr": {
                    "description": "A MapLibre expression giving each feature's pattern icon name (MapLibre data-driven `line-pattern`), cropped per group from the `sprite` atlas. Used instead of an `image` input; a group whose name doesn't resolve to an icon in the sheet is not drawn.",
                },
                "width-px": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0,
                              "description": "Stroke width; the image height is fit to it. 0 = native height." })),
                "width-expr": {
                    "description": "A MapLibre number expression (px, e.g. [\"interpolate\", [\"linear\"], [\"zoom\"], 10, 2, 16, 8]) giving the stroke width, evaluated per feature group with the tile's zoom; overrides the constant `width-px`. A group whose expression doesn't resolve to a number falls back to `width-px`.",
                },
                "spacing-px": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0,
                                "description": "Advance between stamps. 0 = the scaled image width (seamless tiling)." })),
                "scale": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0,
                           "description": "Extra uniform scale on top of the width fit. Default 1.0." })),
                "opacity": schema_frag::unit_number(),
                "opacity-expr": {
                    "description": "A MapLibre number expression giving opacity, evaluated per feature group; overrides the constant `opacity`. A group whose expression doesn't resolve to a number falls back to `opacity`.",
                },
            },
            "required": ["features"],
        })
    }
}

ezu_graph::submit_node!(LineStampFactory);
