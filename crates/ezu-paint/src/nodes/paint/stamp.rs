//! `stamp` — `Features + (Raster|Sprite) -> Raster`. Place the input
//! image once at every point in `Features.points`, with optional
//! rotation / scale / per-point jitter. The image is sampled at its
//! native dimensions regardless of which kind it was wired in as.
//! `anchor` picks which part of the image sits on the point and `offset`
//! shifts it from there, as MapLibre's `icon-anchor` and `icon-offset` do.
//!
//! Lines and polygons in the features input are ignored. The output
//! has the canvas-padded dimensions, matching every other paint node.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ezu_graph::{
    enum_names, schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError,
    FactoryCtx, FactoryError, In, InEnum, InReader, InfluenceCtx, Node, NodeFactory, PortKind,
    PortSpec, PortValue,
};
use serde_json::Value;
use tiny_skia::{PixmapPaint, Transform};
use xxhash_rust::xxh3::Xxh3;

use ezu_core::{
    coord::tile_px_to_world,
    seed::{next_unit, world_seed},
    text::Anchor,
};

use crate::nodes::common::ANCHOR_NAMES as TEXT_ANCHORS;
use crate::nodes::common::{
    canvas_into_raster, core_tile, downcast_features, empty_raster, make_canvas, read_xy,
    unwrap_raster_or_sprite, FramedImage, ACCEPTS_RASTER_OR_SPRITE,
};
use crate::nodes::source::sprite::{sheet_port, sheet_schema, IconCrops, SheetInput};

const STAMP_SALT: u32 = 0x5354_4d50; // 'STMP'
/// Position jitter draws from its own salt, so moving a point around is
/// uncorrelated with how the same point is scaled and rotated.
const STAMP_POS_SALT: u32 = 0x5350_4f53; // 'SPOS'

/// Parse an optional raw MapLibre expression field, type-checked against
/// `expect`. Returns `(parsed, raw_json_text)` for a stable cache hash.
pub(super) fn parse_expr_field(
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
pub(super) fn eval_number(
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

/// [`eval_number`] for a length in style px: a number the expression
/// resolves to is converted to device px, while `fallback` is taken to be
/// in device px already.
pub(super) fn eval_px(
    expr: &Option<maplibre_expr::Expr>,
    ectx: &maplibre_expr::EvaluationContext,
    fallback: f32,
    ctx: &EvalCtx<'_>,
) -> f32 {
    match expr {
        Some(e) => match maplibre_expr::evaluate(e, ectx) {
            Ok(maplibre_expr::Value::Number(n)) => ctx.px32(n as f32),
            _ => fallback,
        },
        None => fallback,
    }
}

/// Evaluate a `Color` expression for a group into straight RGBA in `0..=1`,
/// falling back to `fallback` when the expression is absent or doesn't
/// resolve to a color. maplibre-expr `Color` stores straight channels in
/// `0..=1`, exactly like a parsed hex literal, so a data-driven color paints
/// the same pixels as the same constant.
pub(super) fn eval_color_f32(
    expr: &Option<maplibre_expr::Expr>,
    ectx: &maplibre_expr::EvaluationContext,
    fallback: [f32; 4],
) -> [f32; 4] {
    match expr {
        Some(e) => match maplibre_expr::evaluate(e, ectx) {
            Ok(maplibre_expr::Value::Color(c)) => [c.r as f32, c.g as f32, c.b as f32, c.a as f32],
            _ => fallback,
        },
        None => fallback,
    }
}

/// Evaluate an expression to the icon name (a string) for a group, or `None`
/// when it doesn't resolve to a (non-empty) string.
pub(super) fn eval_icon_name(
    expr: &maplibre_expr::Expr,
    ectx: &maplibre_expr::EvaluationContext,
) -> Option<String> {
    match maplibre_expr::evaluate(expr, ectx) {
        Ok(maplibre_expr::Value::String(s)) if !s.is_empty() => Some(s),
        _ => None,
    }
}

struct StampNode {
    scale: In<f64>,
    rotation_deg: In<f64>,
    rotation_jitter_deg: In<f64>,
    scale_jitter: In<f64>,
    /// Maximum per-point offset in style pixels, drawn independently per
    /// axis. A plain `In<f64>` rather than a `PaddingIn`: this node already
    /// leaves the sprite's own half-extent — usually far larger than a jitter
    /// of a few pixels — to the document's `pad`, so bounding the jitter alone
    /// would buy nothing and would cost `@node` ports their freedom.
    position_jitter_px: In<f64>,
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
    /// Data-driven `icon-image`: instead of the `image` port, a sprite sheet
    /// plus a MapLibre expression giving each feature's icon name. The sheet
    /// is cropped per group by the evaluated name (cropping any icon it
    /// contains), so no `image` input is wired. Absent → the node stamps its
    /// single `image` input.
    icon_sheet: Option<SheetInput>,
    icon_name_expr: Option<maplibre_expr::Expr>,
    icon_name_expr_src: Option<String>,
    /// Crops the icons `icon_name_expr` names out of the sheet.
    icon_crops: IconCrops,
    /// Which part of the image sits on the point (MapLibre `icon-anchor`).
    anchor: InEnum<Anchor>,
    /// Data-driven `anchor`: a MapLibre string expression naming the anchor
    /// per feature group, with its raw JSON text for the hash.
    anchor_expr: Option<maplibre_expr::Expr>,
    anchor_expr_src: Option<String>,
    /// Set once `anchor-expr` has given a name that is not an anchor, so the
    /// mistake is reported once per node rather than once per group.
    anchor_expr_warned: AtomicBool,
    /// Shift `[x, y]` in px from the anchored position, right and down
    /// positive (MapLibre `icon-offset`). Like the image it is scaled by the
    /// stamp's scale and turned with its rotation.
    offset: [f32; 2],
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl StampNode {
    /// The anchor fractions for one group: `anchor-expr`'s anchor when it
    /// names one, else `anchor`. An evaluation error falls back quietly like
    /// the other per-group expressions; a string that names no anchor is a
    /// style mistake, so it is reported, once per node.
    fn group_anchor(&self, ectx: &maplibre_expr::EvaluationContext, anchor: Anchor) -> (f32, f32) {
        let Some(expr) = &self.anchor_expr else {
            return anchor.fraction();
        };
        let Ok(maplibre_expr::Value::String(name)) = maplibre_expr::evaluate(expr, ectx) else {
            return anchor.fraction();
        };
        match Anchor::parse(&name) {
            Some(a) => a.fraction(),
            None => {
                if !self.anchor_expr_warned.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        "stamp: `anchor-expr` gave `{name}`, which is not an anchor name — \
                         `anchor` was used for such points"
                    );
                }
                anchor.fraction()
            }
        }
    }
}

impl Node for StampNode {
    fn op_name(&self) -> &'static str {
        "stamp"
    }

    /// Nothing upstream of this op may be dropped.
    ///
    /// The sprite's own size is only known once the image is loaded,
    /// and `position-jitter-px` has no declared ceiling.
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
        if !feats.has_points() {
            return Ok(empty_raster(ctx));
        }

        // Constants, resolved once. Data-driven exprs (if present) override
        // these per feature group; whichever expr is absent uses the constant.
        let const_scale = (self.scale.get(ctx, inputs)? as f32).max(0.0);
        let const_rotation_deg = self.rotation_deg.get(ctx, inputs)? as f32;
        let rotation_jitter_deg = self.rotation_jitter_deg.get(ctx, inputs)? as f32;
        let scale_jitter = self.scale_jitter.get(ctx, inputs)? as f32;
        // A sprite's pixels, its offset and the jitter are style px, drawn in
        // device px: the jitter converts here, the sprite and its offset
        // through the scale it is drawn at.
        let position_jitter_px = ctx.px32(self.position_jitter_px.get(ctx, inputs)? as f32);
        let const_opacity = (self.opacity.get(ctx, inputs)? as f32).clamp(0.0, 1.0);
        let const_anchor = self.anchor.get(ctx)?;

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
        let tile = core_tile(ctx);
        let world_at = |px: f32, py: f32| {
            // The pad is the margin above and left of the tile, so
            // subtracting it gives tile-local pixels.
            tile_px_to_world(
                tile,
                px as f64 - pad as f64,
                py as f64 - pad as f64,
                tile_w as f64,
                tile_h as f64,
            )
        };

        // Stamp every point in `points` with `img` at the given scale /
        // rotation / opacity, the `anchor` fractions of the image on the
        // point. Jitter is keyed by world position, so it does not depend on
        // the group the point came from.
        let offset = self.offset;
        let stamp_points = |pm: &mut tiny_skia::Pixmap,
                            points: &[(i32, i32)],
                            img: &FramedImage,
                            scale: f32,
                            rotation_deg: f32,
                            (fx, fy): (f32, f32),
                            pix_paint: &PixmapPaint| {
            let (iw, ih) = img.size();
            let mut pm = pm.as_mut();
            for &(x, y) in points {
                let px = x as f32 * sx + pad;
                let py = y as f32 * sy + pad;
                let w = world_at(px, py);

                let (mut rot_off, mut scale_off) = (0.0_f32, 0.0_f32);
                if rotation_jitter_deg != 0.0 || scale_jitter != 0.0 {
                    let mut seed = world_seed(w, STAMP_SALT);
                    rot_off = (next_unit(&mut seed) - 0.5) * 2.0 * rotation_jitter_deg;
                    scale_off = (next_unit(&mut seed) - 0.5) * 2.0 * scale_jitter;
                }
                let s = (scale * (1.0 + scale_off)).max(0.0);
                if s <= 0.0 {
                    continue;
                }
                // Offset the point itself, keyed by its *unjittered* world
                // position so a stamp straddling a tile edge lands in the same
                // place in both tiles.
                let (mut dx, mut dy) = (0.0_f32, 0.0_f32);
                if position_jitter_px != 0.0 {
                    let mut seed = world_seed(w, STAMP_POS_SALT);
                    dx = (next_unit(&mut seed) - 0.5) * 2.0 * position_jitter_px;
                    dy = (next_unit(&mut seed) - 0.5) * 2.0 * position_jitter_px;
                }
                // The image turns and scales about the point, and the offset
                // with it, as MapLibre turns an icon about its anchor.
                let s = ctx.px32(s);
                let t = Transform::from_translate(px + dx, py + dy)
                    .pre_rotate(rotation_deg + rot_off)
                    .pre_scale(s, s)
                    .pre_translate(offset[0] - fx * iw, offset[1] - fy * ih);
                img.draw(&mut pm, pix_paint, t);
            }
        };
        let has_paint_expr = self.scale_expr.is_some()
            || self.rotation_deg_expr.is_some()
            || self.opacity_expr.is_some()
            || self.anchor_expr.is_some();
        let z = ctx.tile.z;
        let pm = canvas.pixmap_mut();

        if let Some(name_expr) = &self.icon_name_expr {
            // Data-driven `icon-image`: crop the feature's icon from the bound
            // sprite sheet per group. Crops are cached by name, and both the
            // paint (scale / rotation / opacity) and the icon are resolved per
            // group from the same feature context.
            let sheet = self
                .icon_sheet
                .as_ref()
                .ok_or_else(|| EvalError::Other("stamp: icon mode without a sheet".into()))?
                .get(ctx, inputs, "sheet")?;
            let mut crops: HashMap<String, Option<FramedImage>> = HashMap::new();
            for group in &feats.groups {
                let ectx = crate::render::group_expr_context(group, z);
                let Some(name) = eval_icon_name(name_expr, &ectx) else {
                    continue;
                };
                let Some(img) = crops.entry(name.clone()).or_insert_with(|| {
                    self.icon_crops
                        .crop("stamp", &sheet, &name)
                        .as_ref()
                        .and_then(FramedImage::new)
                }) else {
                    continue;
                };
                let scale = eval_number(&self.scale_expr, &ectx, const_scale).max(0.0);
                let rotation_deg = eval_number(&self.rotation_deg_expr, &ectx, const_rotation_deg);
                let opacity = eval_number(&self.opacity_expr, &ectx, const_opacity).clamp(0.0, 1.0);
                let pix_paint = PixmapPaint {
                    opacity,
                    ..PixmapPaint::default()
                };
                let anchor = self.group_anchor(&ectx, const_anchor);
                stamp_points(
                    pm,
                    &group.points,
                    img,
                    scale,
                    rotation_deg,
                    anchor,
                    &pix_paint,
                );
            }
        } else {
            // Single `image` input stamped at every point.
            let image_in = inputs[1]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("image".into()))?;
            let (image, _) = unwrap_raster_or_sprite(image_in, "image")?;
            let Some(img) = FramedImage::new(&image) else {
                return Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))));
            };
            if has_paint_expr {
                for group in &feats.groups {
                    let ectx = crate::render::group_expr_context(group, z);
                    let scale = eval_number(&self.scale_expr, &ectx, const_scale).max(0.0);
                    let rotation_deg =
                        eval_number(&self.rotation_deg_expr, &ectx, const_rotation_deg);
                    let opacity =
                        eval_number(&self.opacity_expr, &ectx, const_opacity).clamp(0.0, 1.0);
                    let pix_paint = PixmapPaint {
                        opacity,
                        ..PixmapPaint::default()
                    };
                    let anchor = self.group_anchor(&ectx, const_anchor);
                    stamp_points(
                        pm,
                        &group.points,
                        &img,
                        scale,
                        rotation_deg,
                        anchor,
                        &pix_paint,
                    );
                }
            } else {
                let pix_paint = PixmapPaint {
                    opacity: const_opacity,
                    ..PixmapPaint::default()
                };
                let points: Vec<(i32, i32)> = feats.points().collect();
                stamp_points(
                    pm,
                    &points,
                    &img,
                    const_scale,
                    const_rotation_deg,
                    const_anchor.fraction(),
                    &pix_paint,
                );
            }
        }

        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn asset_inputs(&self) -> Vec<String> {
        // The deprecated `sprite` field loads the sheet here.
        self.icon_sheet
            .iter()
            .filter_map(SheetInput::asset_inputs)
            .collect()
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"stamp");
        self.scale.param_hash(h);
        self.rotation_deg.param_hash(h);
        self.rotation_jitter_deg.param_hash(h);
        self.scale_jitter.param_hash(h);
        self.position_jitter_px.param_hash(h);
        self.opacity.param_hash(h);
        for (tag, src) in [
            (b"scaleexpr".as_slice(), &self.scale_expr_src),
            (b"rotationdegexpr".as_slice(), &self.rotation_deg_expr_src),
            (b"opacityexpr".as_slice(), &self.opacity_expr_src),
            (b"iconnameexpr".as_slice(), &self.icon_name_expr_src),
            (b"anchorexpr".as_slice(), &self.anchor_expr_src),
        ] {
            if let Some(s) = src {
                h.update(tag);
                h.update(s.as_bytes());
            }
        }
        if let Some(sprite) = self.icon_sheet.as_ref().and_then(SheetInput::field_key) {
            h.update(b"iconsprite");
            h.update(sprite.as_bytes());
        }
        self.anchor.param_hash(h);
        h.update(&self.offset[0].to_le_bytes());
        h.update(&self.offset[1].to_le_bytes());
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

/// A data-driven icon, read by [`read_icon_name_expr`]: where the sheet
/// comes from, the connection wiring its port (if it is one), and the
/// compiled `name-expr` with its raw JSON text.
pub(super) struct IconByName {
    pub(super) sheet: SheetInput,
    pub(super) connection: Option<Connection>,
    pub(super) expr: maplibre_expr::Expr,
    pub(super) expr_src: String,
}

/// Read a data-driven icon: a `name-expr` (the MapLibre expression naming
/// each feature's icon) plus the sheet it crops from — a `sprite` node wired
/// to `sheet`, or the deprecated `sprite` field naming the source. The two
/// replace an `image` input: any icon in the sheet is cropped per feature
/// group, so no `image` node is wired. The sheet's port, when wired, is input
/// 1. `None` without `name-expr`.
pub(super) fn read_icon_name_expr(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
) -> Result<Option<IconByName>, FactoryError> {
    let Some(v) = fields.get("name-expr") else {
        return Ok(None);
    };
    let (sheet, connection) = SheetInput::read(fields, ctx, "sheet", "sprite", 1)?
        .ok_or_else(|| FactoryError::MissingField("sheet".into()))?;
    let expr = maplibre_expr::parse(v).map_err(|e| FactoryError::BadField {
        field: "name-expr".into(),
        msg: e.to_string(),
    })?;
    // Prefer a String check (icon names stringify); fall back to untyped,
    // since `match`/`step` over literals may not narrow.
    let expr = maplibre_expr::typecheck(&expr, Some(&maplibre_expr::Type::String), false)
        .or_else(|_| maplibre_expr::typecheck(&expr, None, false))
        .map_err(|e| FactoryError::BadField {
            field: "name-expr".into(),
            msg: e.to_string(),
        })?;
    Ok(Some(IconByName {
        sheet,
        connection,
        expr,
        expr_src: v.to_string(),
    }))
}

/// The ports and connections a stamping op starts with: `features`, then
/// the sheet when `icon` names icons from a wired one, or else `image`.
pub(super) fn image_ports(
    fields: &serde_json::Map<String, Value>,
    icon: &mut Option<IconByName>,
) -> Result<(Vec<PortSpec>, Vec<Connection>), FactoryError> {
    let mut ports = vec![PortSpec {
        name: "features",
        accepts: &[PortKind::Features],
        optional: false,
    }];
    let mut connections = vec![Connection {
        port: "features".into(),
        src: take_input_ref(fields, "features")?,
    }];
    match icon {
        Some(icon) => {
            if let Some(c) = icon.connection.take() {
                ports.push(sheet_port("sheet"));
                connections.push(c);
            }
        }
        None => {
            ports.push(PortSpec {
                name: "image",
                accepts: ACCEPTS_RASTER_OR_SPRITE,
                optional: false,
            });
            connections.push(Connection {
                port: "image".into(),
                src: take_input_ref(fields, "image")?,
            });
        }
    }
    Ok((ports, connections))
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
        // `features`, then either the sheet the icons are named from or the
        // single `image`.
        let mut icon = read_icon_name_expr(fields, ctx)?;
        let (mut ports, mut connections) = image_ports(fields, &mut icon)?;

        let mut r = InReader::new(fields, ctx, ports.len());
        let scale = r.number_or("scale", 1.0)?;
        let rotation_deg = r.number_or("rotation-deg", 0.0)?;
        let rotation_jitter_deg = r.number_or("rotation-jitter-deg", 0.0)?;
        let scale_jitter = r.number_or("scale-jitter", 0.0)?;
        let position_jitter_px = r.number_or("position-jitter-px", 0.0)?;
        let opacity = r.number_or("opacity", 1.0)?;
        let anchor = r.enum_or("anchor", Anchor::Center, TEXT_ANCHORS)?;
        let parts = r.finish();

        let (scale_expr, scale_expr_src) =
            parse_expr_field(fields, "scale-expr", &maplibre_expr::Type::Number)?;
        let (rotation_deg_expr, rotation_deg_expr_src) =
            parse_expr_field(fields, "rotation-deg-expr", &maplibre_expr::Type::Number)?;
        let (opacity_expr, opacity_expr_src) =
            parse_expr_field(fields, "opacity-expr", &maplibre_expr::Type::Number)?;
        let (anchor_expr, anchor_expr_src) =
            parse_expr_field(fields, "anchor-expr", &maplibre_expr::Type::String)?;
        let offset = read_xy(fields, "offset", ctx, [0.0, 0.0])?;

        ports.extend(parts.ports);
        connections.extend(parts.connections);
        let (icon_sheet, icon_name_expr, icon_name_expr_src) = match icon {
            Some(i) => (Some(i.sheet), Some(i.expr), Some(i.expr_src)),
            None => (None, None, None),
        };

        Ok(BuiltNode {
            node: Box::new(StampNode {
                scale,
                rotation_deg,
                rotation_jitter_deg,
                scale_jitter,
                position_jitter_px,
                opacity,
                scale_expr,
                rotation_deg_expr,
                opacity_expr,
                scale_expr_src,
                rotation_deg_expr_src,
                opacity_expr_src,
                icon_sheet,
                icon_name_expr,
                icon_name_expr_src,
                icon_crops: IconCrops::default(),
                anchor,
                anchor_expr,
                anchor_expr_src,
                anchor_expr_warned: AtomicBool::new(false),
                offset,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Stamp a sprite at every input point. Lines and polygons are ignored. Rotation, scale and position jitter are world-deterministic — a given point gets the same jitter no matter which tile renders it, and the three do not correlate. Provide either an `image` input (one sprite for every point) or, for a data-driven `icon-image`, a `sheet` input from a `sprite` node plus a `name-expr` that names each feature's icon.",
            "properties": {
                "features": schema_frag::node_ref(),
                "image": schema_frag::node_ref(),
                "sheet": sheet_schema("A `sprite` node: the sheet `name-expr` names icons from."),
                "sprite": { "type": "string",
                            "description": "Deprecated: load the sheet with a `sprite` node and wire it to `sheet`. An `@sprite-source` ref the node loads the sheet from itself; ignored when `sheet` is wired." },
                "name-expr": {
                    "description": "A MapLibre expression giving each feature's icon name (MapLibre data-driven `icon-image`), cropped per group from the `sheet`. Used instead of an `image` input; any icon the sheet contains is croppable, so no enumeration is needed. A name the sheet lacks stamps nothing.",
                },
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
                "position-jitter-px": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0,
                                        "description": "Per-point random offset, ±value pixels, drawn independently on x and y. Breaks up the regular lattice a `point-grid` leaves. Default 0 (no offset)." })),
                "opacity": schema_frag::unit_number(),
                "opacity-expr": {
                    "description": "A MapLibre number expression giving opacity, evaluated per feature group; overrides the constant `opacity`.",
                },
                "anchor": schema_frag::in_enum(serde_json::json!({ "type": "string", "enum": enum_names(TEXT_ANCHORS),
                            "description": "Which part of the sprite sits on the point (MapLibre `icon-anchor`): `bottom` puts its bottom edge's middle there, say, for a pin. Rotation and scale turn about the point. Default `center`." })),
                "anchor-expr": {
                    "description": "A MapLibre string expression naming the anchor per feature group (data-driven `icon-anchor`): one of the `anchor` names. It takes the place of `anchor` for that group; a group whose result is not an anchor name, or whose evaluation fails, uses `anchor`, and a result that is not an anchor name is reported once per node.",
                },
                "offset": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2,
                            "description": "Shift [x, y] in px from the anchored position, right and down positive (MapLibre `icon-offset`). Like the sprite it is scaled by `scale` and turned by the rotation. Default [0, 0]." },
            },
            "required": ["features"],
        })
    }
}

ezu_graph::submit_node!(StampFactory);
