//! `text` — `Features -> Raster`. Draw a text label at feature points,
//! shaped and laid out by `ezu-core`'s `text` module (MapLibre point
//! placement) with deterministic cross-tile collision.
//!
//! `font` names an ordered fallback stack of `font` and/or `glyphs`
//! sources from the document's `sources` block — outline font files
//! and MapLibre SDF glyph endpoints mix freely; the first entry
//! covering a char shapes it. `text` is a constant string or a raw
//! MapLibre string expression evaluated per feature group; `size` /
//! `color` / `halo-color` / `halo-width` / `opacity` follow the usual
//! constant-plus-`*-expr`-sibling pattern. Layout knobs (anchor,
//! justify, wrapping, spacing) are build-time constants.
//!
//! Lines and polygons in the features input are ignored. Drawing is a
//! pure function of world position (no jitter), so labels match across
//! tile borders. Collision (default on) is likewise world-space
//! deterministic: candidates are gathered from this tile plus its 8
//! neighbour tiles (host-bound under `<source>.<layer>@dx,dy`), deduped
//! by `(text, quantized world anchor)`, ordered by
//! `(sort-key, world anchor, text)`, and placed greedily against a grid
//! — all from world-space quantities, so adjacent tiles agree on every
//! straddling label. Missing neighbour bindings degrade to centre-only.
//! Divergence from MapLibre is deliberate and not emulated: no
//! viewport-centre priority and no per-frame fade in/out. See
//! [`ezu_core::text::collide`].

use std::collections::HashMap;
use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, Asset, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError,
    FactoryCtx, FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use ezu_core::text::{
    collide::{self, Aabb, Candidate},
    draw, layout, Anchor, Font, Justify, LayoutParams, SdfFontStack, StackEntry, TextBlock,
    TextPaint, TextTransform,
};
use ezu_features::FeatureLayer;

use crate::nodes::common::{
    canvas_into_raster, downcast_features, empty_raster, make_canvas, read_bool_or, read_number_or,
    read_optional_string, read_string_or, read_xy, FeatureGroup,
};
use crate::render::collect_groups;

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

/// Evaluate a `Color` expression for a group into straight RGBA (`0..=1`
/// components, the repo color convention), falling back to `fallback`
/// when absent or non-color.
fn eval_color(
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

struct TextNode {
    /// Font asset keys in fallback order: a `font` source's `url`, or a
    /// `glyphs` source's asset key (its `{range}` URL template).
    font_keys: Vec<String>,
    /// Constant label; `None` when `text` is an expression.
    text: Option<String>,
    /// Data-driven label: a MapLibre string expression evaluated per
    /// feature group. A group whose result is empty (or errors) draws
    /// nothing.
    text_expr: Option<maplibre_expr::Expr>,
    size: In<f64>,
    color: In<[f32; 4]>,
    halo_color: In<[f32; 4]>,
    halo_width: In<f64>,
    opacity: In<f64>,
    /// Optional data-driven overrides, MapLibre expressions evaluated
    /// per feature group; each overrides its constant counterpart.
    size_expr: Option<maplibre_expr::Expr>,
    color_expr: Option<maplibre_expr::Expr>,
    halo_color_expr: Option<maplibre_expr::Expr>,
    halo_width_expr: Option<maplibre_expr::Expr>,
    opacity_expr: Option<maplibre_expr::Expr>,
    /// Raw expression JSON text, for a stable hash.
    text_expr_src: Option<String>,
    size_expr_src: Option<String>,
    color_expr_src: Option<String>,
    halo_color_expr_src: Option<String>,
    halo_width_expr_src: Option<String>,
    opacity_expr_src: Option<String>,
    /// Build-time layout constants (em units except where noted).
    anchor: Anchor,
    justify: Justify,
    transform: TextTransform,
    offset_em: [f32; 2],
    max_width_em: f32,
    line_height: f32,
    letter_spacing_em: f32,
    /// Labels whose rendered bbox half-extent exceeds this many px are
    /// culled (they'd overflow the canvas pad this node requested).
    max_extent_px: f32,
    // --- Collision (deterministic cross-tile placement) ---
    /// Whether to run collision. MapLibre's default is on; `false`
    /// restores the draw-everything behaviour (every label draws).
    collide: bool,
    /// MapLibre `text-allow-overlap`: place regardless of collision.
    allow_overlap: bool,
    /// MapLibre `text-ignore-placement`: don't block later labels.
    ignore_placement: bool,
    /// MapLibre `text-padding`: collision box inflation in px.
    padding_px: f32,
    /// MapLibre `symbol-sort-key`: per-feature number; lower places
    /// first. Absent = 0.
    sort_key_expr: Option<maplibre_expr::Expr>,
    sort_key_expr_src: Option<String>,
    /// The upstream `<source>.<layer>` asset name, used only to spell the
    /// neighbour binding names in [`Node::asset_inputs`] (the translator
    /// always sets `source`/`layer`; hand-written recipes may omit them →
    /// centre-only collision). `None` disables neighbour gathering.
    neighbor_base: Option<String>,
    /// The upstream feature filter, reproduced when gathering neighbour
    /// candidates so they are filtered identically to the tile's own
    /// features (the translator copies the `features` node's filter here).
    filter_expr: Option<maplibre_expr::Expr>,
    filter_expr_src: Option<String>,
    min_zoom_field: Option<String>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl TextNode {
    fn layout_params(&self) -> LayoutParams {
        LayoutParams {
            max_width_em: self.max_width_em,
            line_height_em: self.line_height,
            letter_spacing_em: self.letter_spacing_em,
            anchor: self.anchor,
            justify: self.justify,
            offset_em: self.offset_em,
            transform: self.transform,
        }
    }
}

impl Node for TextNode {
    fn op_name(&self) -> &'static str {
        "text"
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
    fn required_pad(&self, downstream: u32) -> u32 {
        downstream + self.max_extent_px.max(0.0).ceil() as u32
    }
    fn asset_inputs(&self) -> Vec<String> {
        let mut keys = self.font_keys.clone();
        // With collision on and a known upstream source, request the 8
        // neighbour layers so a host that can fetch them binds them for
        // cross-tile placement. Unbound neighbours degrade to centre-only.
        if self.collide {
            if let Some(base) = &self.neighbor_base {
                keys.extend(ezu_graph::neighbor_bindings(base));
            }
        }
        keys
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
        // With collision off and no own points there is nothing to draw.
        // With collision on, neighbour tiles may still spill labels into
        // this tile, so we proceed and decide from the gathered candidates.
        if !self.collide && !feats.has_points() {
            return Ok(empty_raster(ctx));
        }

        // Resolve the font stack once per eval. Outline fonts and SDF
        // glyph stacks share the fallback stack; the draw path is
        // picked per glyph by its entry's backend.
        let mut fonts: Vec<StackEntry> = Vec::with_capacity(self.font_keys.len());
        for key in &self.font_keys {
            let entry = match ctx.assets.load(key)? {
                Asset::Font(opq) => StackEntry::Outline(opq.downcast::<Font>().map_err(|_| {
                    EvalError::Other(format!("`{key}` payload is not a text Font"))
                })?),
                Asset::Glyphs(opq) => {
                    StackEntry::Sdf(opq.downcast::<SdfFontStack>().map_err(|_| {
                        EvalError::Other(format!("`{key}` payload is not an SdfFontStack"))
                    })?)
                }
                _ => {
                    return Err(EvalError::Other(format!(
                        "asset `{key}` is not a font or glyphs source"
                    )))
                }
            };
            fonts.push(entry);
        }

        // Constants, resolved once. Data-driven exprs (if present)
        // override these per feature group.
        let const_size = (self.size.get(ctx, inputs)? as f32).max(0.0);
        let const_color = self.color.get(ctx, inputs)?;
        let const_halo_color = self.halo_color.get(ctx, inputs)?;
        let const_halo_width = (self.halo_width.get(ctx, inputs)? as f32).max(0.0);
        let const_opacity = (self.opacity.get(ctx, inputs)? as f32).clamp(0.0, 1.0);

        let mut canvas = make_canvas(ctx)?;
        let pad = canvas.pad() as f32;
        let tile_w = canvas.tile_width() as f32;
        let tile_h = canvas.tile_height() as f32;
        let extent_i = feats.extent.max(1) as i64;
        let sx = tile_w / extent_i as f32;
        let sy = tile_h / extent_i as f32;
        let z = ctx.tile.z;
        let params = self.layout_params();
        let (tx, ty) = (ctx.tile.x as i64, ctx.tile.y as i64);

        // Shaping is the expensive step; the same (text, size) pair is
        // laid out once per eval no matter how many groups/points repeat
        // it. Neighbour candidates share this cache (identical exprs →
        // identical layout across tiles).
        let mut blocks: HashMap<(String, u32), Arc<TextBlock>> = HashMap::new();
        let mut culled = 0usize;
        let mut dropped_chars = 0usize;
        let mut missing_range_chars = 0usize;

        /// One placed label's draw payload, index-aligned with the
        /// collision candidate list.
        struct DrawRec {
            block: Arc<TextBlock>,
            anchor: (f32, f32),
            paint: TextPaint,
        }
        let mut cands: Vec<Candidate> = Vec::new();
        let mut draws: Vec<DrawRec> = Vec::new();

        // Turn one feature group's points into placement candidates,
        // evaluated at neighbour offset `(dx, dy)` (`(0, 0)` for the
        // tile's own features). Everything that feeds the collision
        // decision is derived from the world anchor (exact integer
        // tile-frame coordinate) and the em box × size — never from
        // tile-local floats — so adjacent tiles agree.
        let mut add_group = |group: &FeatureGroup, dx: i64, dy: i64| {
            if group.points.is_empty() {
                return;
            }
            let ectx = crate::render::group_expr_context(group, z);
            let text = match &self.text_expr {
                Some(e) => match maplibre_expr::evaluate(e, &ectx) {
                    Ok(maplibre_expr::Value::String(s)) => s,
                    _ => return,
                },
                None => self.text.clone().unwrap_or_default(),
            };
            if text.is_empty() {
                return;
            }
            let size = eval_number(&self.size_expr, &ectx, const_size).max(0.0);
            if size <= 0.0 {
                return;
            }
            let sort_key = match &self.sort_key_expr {
                Some(e) => match maplibre_expr::evaluate(e, &ectx) {
                    Ok(maplibre_expr::Value::Number(n)) => n,
                    _ => 0.0,
                },
                None => 0.0,
            };
            let opacity = eval_number(&self.opacity_expr, &ectx, const_opacity).clamp(0.0, 1.0);
            let mut color = eval_color(&self.color_expr, &ectx, const_color);
            let mut halo_color = eval_color(&self.halo_color_expr, &ectx, const_halo_color);
            color[3] *= opacity;
            halo_color[3] *= opacity;
            let halo_width = eval_number(&self.halo_width_expr, &ectx, const_halo_width).max(0.0);

            let block = blocks
                .entry((text.clone(), size.to_bits()))
                .or_insert_with(|| Arc::new(layout(&text, &fonts, &params)))
                .clone();
            // Count layout warnings once per distinct label (own groups
            // only; neighbours repeat the same strings).
            if dx == 0 && dy == 0 {
                dropped_chars += block.dropped_chars;
                missing_range_chars += block.missing_range_chars;
            }
            if block.is_empty() {
                return;
            }
            // A label reaching past the pad this node requested would clip
            // at tile borders — cull it instead.
            let b = block.bbox;
            let half_extent = [b.min_x, b.max_x, b.min_y, b.max_y]
                .iter()
                .fold(0.0f32, |m, v| m.max(v.abs()))
                * size;
            if half_extent > self.max_extent_px {
                if dx == 0 && dy == 0 {
                    culled += group.points.len();
                }
                return;
            }
            let paint = TextPaint {
                size_px: size,
                color,
                halo_color,
                halo_width_px: halo_width,
                halo_blur_px: 0.0,
            };
            for &(x, y) in &group.points {
                let world_ax = (tx + dx) * extent_i + x as i64;
                let world_ay = (ty + dy) * extent_i + y as i64;
                // Local world-pixel frame (current tile origin subtracted):
                // small magnitudes, and translation-invariant so a
                // neighbour tile — which subtracts its own origin — reaches
                // identical collision decisions.
                let lpx = (world_ax - tx * extent_i) as f32 * sx;
                let lpy = (world_ay - ty * extent_i) as f32 * sy;
                let aabb = Aabb {
                    min_x: lpx + b.min_x * size,
                    min_y: lpy + b.min_y * size,
                    max_x: lpx + b.max_x * size,
                    max_y: lpy + b.max_y * size,
                }
                .inflate(self.padding_px);
                cands.push(Candidate {
                    sort_key,
                    world_ax,
                    world_ay,
                    text: text.clone(),
                    aabb,
                    allow_overlap: self.allow_overlap,
                    ignore_placement: self.ignore_placement,
                });
                draws.push(DrawRec {
                    block: block.clone(),
                    anchor: (lpx + pad, lpy + pad),
                    paint,
                });
            }
        };

        // The tile's own features.
        for group in &feats.groups {
            add_group(group, 0, 0);
        }
        // Neighbour candidates (cross-tile placement): re-read each bound
        // neighbour layer, re-apply the same filter, and evaluate the same
        // expressions — so both tiles sharing a border produce identical
        // candidates. Unbound neighbours simply contribute nothing.
        if self.collide {
            if let Some(base) = &self.neighbor_base {
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let name = ezu_graph::neighbor_binding(base, dx, dy);
                        let layer = match ctx.assets.load(&name) {
                            Ok(Asset::Features(opq)) => opq.downcast::<FeatureLayer>().ok(),
                            _ => None,
                        };
                        let Some(layer) = layer else { continue };
                        // Mismatched extent would break the shared world frame.
                        if layer.extent.max(1) as i64 != extent_i {
                            continue;
                        }
                        let groups = collect_groups(
                            &layer.features,
                            self.filter_expr.as_ref(),
                            &self.min_zoom_field,
                            z,
                        );
                        for group in &groups {
                            add_group(group, dx as i64, dy as i64);
                        }
                    }
                }
            }
        }

        // Deterministic placement (or draw-everything when collision off).
        let placed: Vec<usize> = if self.collide {
            collide::place(&cands, collide::COLLISION_CELL_PX)
        } else {
            (0..cands.len()).collect()
        };

        let padded_w = tile_w + 2.0 * pad;
        let padded_h = tile_h + 2.0 * pad;
        let pm = canvas.pixmap_mut();
        let mut pm = pm.as_mut();
        for &i in &placed {
            let d = &draws[i];
            // Draw only placed labels whose box touches this padded canvas
            // (a neighbour's winner may sit entirely outside it).
            if self.collide {
                let bb = d.block.bbox;
                let s = d.paint.size_px;
                let (ax, ay) = d.anchor;
                let min_x = ax + bb.min_x * s - self.padding_px;
                let max_x = ax + bb.max_x * s + self.padding_px;
                let min_y = ay + bb.min_y * s - self.padding_px;
                let max_y = ay + bb.max_y * s + self.padding_px;
                if max_x < 0.0 || min_x > padded_w || max_y < 0.0 || min_y > padded_h {
                    continue;
                }
            }
            draw(&d.block, &fonts, &mut pm, d.anchor, &d.paint);
        }
        // One summary line per eval, not one per label.
        if culled > 0 {
            tracing::warn!(
                "text: culled {culled} label placement(s) whose bbox exceeds max-extent-px ({}px)",
                self.max_extent_px
            );
        }
        if dropped_chars > 0 {
            tracing::warn!(
                "text: {dropped_chars} char(s) not covered by the font stack were dropped"
            );
        }
        if missing_range_chars > 0 {
            tracing::warn!(
                "text: {missing_range_chars} of the dropped char(s) hit glyph ranges that were \
                 unavailable — a host without lazy fetching (wasm) must bind every needed range \
                 up front"
            );
        }

        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"text");
        for key in &self.font_keys {
            h.update(key.as_bytes());
            h.update(&[0]);
        }
        if let Some(t) = &self.text {
            h.update(b"const");
            h.update(t.as_bytes());
        }
        self.size.param_hash(h);
        self.color.param_hash(h);
        self.halo_color.param_hash(h);
        self.halo_width.param_hash(h);
        self.opacity.param_hash(h);
        for (tag, src) in [
            (b"textexpr".as_slice(), &self.text_expr_src),
            (b"sizeexpr".as_slice(), &self.size_expr_src),
            (b"colorexpr".as_slice(), &self.color_expr_src),
            (b"halocolorexpr".as_slice(), &self.halo_color_expr_src),
            (b"halowidthexpr".as_slice(), &self.halo_width_expr_src),
            (b"opacityexpr".as_slice(), &self.opacity_expr_src),
            (b"sortkeyexpr".as_slice(), &self.sort_key_expr_src),
            (b"filterexpr".as_slice(), &self.filter_expr_src),
        ] {
            if let Some(s) = src {
                h.update(tag);
                h.update(s.as_bytes());
            }
        }
        h.update(&[self.anchor as u8, self.justify as u8, self.transform as u8]);
        for v in [
            self.offset_em[0],
            self.offset_em[1],
            self.max_width_em,
            self.line_height,
            self.letter_spacing_em,
            self.max_extent_px,
            self.padding_px,
        ] {
            h.update(&v.to_le_bytes());
        }
        h.update(&[
            self.collide as u8,
            self.allow_overlap as u8,
            self.ignore_placement as u8,
        ]);
        if let Some(base) = &self.neighbor_base {
            h.update(b"nbase");
            h.update(base.as_bytes());
        }
        if let Some(f) = &self.min_zoom_field {
            h.update(b"mzf");
            h.update(f.as_bytes());
        }
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct TextFactory;
impl NodeFactory for TextFactory {
    fn op_name(&self) -> &'static str {
        "text"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;

        // `font`: an ordered array of `font` / `glyphs` source names —
        // the fallback stack. Each resolves to its source's asset key
        // (a font's `url`; a glyphs source's `{range}` URL template).
        let font_field = fields
            .get("font")
            .ok_or_else(|| FactoryError::MissingField("font".into()))?;
        let names = font_field
            .as_array()
            .ok_or_else(|| FactoryError::BadField {
                field: "font".into(),
                msg: "expected an array of font source names".into(),
            })?;
        if names.is_empty() {
            return Err(FactoryError::BadField {
                field: "font".into(),
                msg: "font stack must name at least one font source".into(),
            });
        }
        let mut font_keys = Vec::with_capacity(names.len());
        for v in names {
            let name = v.as_str().ok_or_else(|| FactoryError::BadField {
                field: "font".into(),
                msg: "font stack entries must be strings".into(),
            })?;
            match ctx.sources.get(name) {
                Some(ezu_style::SourceDecl::Font(f)) => font_keys.push(f.url.clone()),
                Some(ezu_style::SourceDecl::Glyphs(g)) => font_keys.push(g.asset_key()),
                Some(_) => {
                    return Err(FactoryError::BadField {
                        field: "font".into(),
                        msg: format!("source `{name}` is not a font or glyphs source"),
                    })
                }
                None => return Err(FactoryError::UnknownAsset(name.to_string())),
            }
        }

        // `text`: a literal string, or a raw MapLibre expression
        // type-checked to String (with top-level coercion, so number /
        // property expressions stringify).
        let (text, text_expr, text_expr_src) = match fields.get("text") {
            None => return Err(FactoryError::MissingField("text".into())),
            Some(Value::String(s)) => (Some(s.clone()), None, None),
            Some(v) => {
                let expr = maplibre_expr::parse(v).map_err(|e| FactoryError::BadField {
                    field: "text".into(),
                    msg: e.to_string(),
                })?;
                let expr =
                    maplibre_expr::typecheck(&expr, Some(&maplibre_expr::Type::String), true)
                        .map_err(|e| FactoryError::BadField {
                            field: "text".into(),
                            msg: e.to_string(),
                        })?;
                (None, Some(expr), Some(v.to_string()))
            }
        };

        let mut r = InReader::new(fields, ctx, 1);
        let size = r.number_or("size", 16.0)?;
        let color = r.color_or("color", [0.0, 0.0, 0.0, 1.0])?;
        let halo_color = r.color_or("halo-color", [1.0, 1.0, 1.0, 1.0])?;
        let halo_width = r.number_or("halo-width", 0.0)?;
        let opacity = r.number_or("opacity", 1.0)?;
        let parts = r.finish();

        let (size_expr, size_expr_src) =
            parse_expr_field(fields, "size-expr", &maplibre_expr::Type::Number)?;
        let (color_expr, color_expr_src) =
            parse_expr_field(fields, "color-expr", &maplibre_expr::Type::Color)?;
        let (halo_color_expr, halo_color_expr_src) =
            parse_expr_field(fields, "halo-color-expr", &maplibre_expr::Type::Color)?;
        let (halo_width_expr, halo_width_expr_src) =
            parse_expr_field(fields, "halo-width-expr", &maplibre_expr::Type::Number)?;
        let (opacity_expr, opacity_expr_src) =
            parse_expr_field(fields, "opacity-expr", &maplibre_expr::Type::Number)?;

        // Layout constants. Enumerated strings are validated here so a
        // typo fails the build instead of silently defaulting.
        let anchor_s = read_string_or(fields, "anchor", ctx, "center")?;
        let anchor = Anchor::parse(&anchor_s).ok_or_else(|| FactoryError::BadField {
            field: "anchor".into(),
            msg: format!("unknown anchor `{anchor_s}`"),
        })?;
        let justify_s = read_string_or(fields, "justify", ctx, "auto")?;
        let justify = Justify::parse(&justify_s).ok_or_else(|| FactoryError::BadField {
            field: "justify".into(),
            msg: format!("unknown justify `{justify_s}` (auto|left|center|right)"),
        })?;
        let transform_s = read_string_or(fields, "transform", ctx, "none")?;
        let transform =
            TextTransform::parse(&transform_s).ok_or_else(|| FactoryError::BadField {
                field: "transform".into(),
                msg: format!("unknown transform `{transform_s}` (none|uppercase|lowercase)"),
            })?;
        let offset_em = read_xy(fields, "offset-em", ctx, [0.0, 0.0])?;
        let max_width_em = read_number_or(fields, "max-width-em", ctx, 10.0)? as f32;
        let line_height = read_number_or(fields, "line-height", ctx, 1.2)? as f32;
        let letter_spacing_em = read_number_or(fields, "letter-spacing-em", ctx, 0.0)? as f32;
        let max_extent_px = read_number_or(fields, "max-extent-px", ctx, 128.0)? as f32;

        // Collision. Default on (MapLibre's default); `collide: false`
        // restores the draw-everything behaviour.
        let collide = read_bool_or(fields, "collide", ctx, true)?;
        let allow_overlap = read_bool_or(fields, "allow-overlap", ctx, false)?;
        let ignore_placement = read_bool_or(fields, "ignore-placement", ctx, false)?;
        let padding_px = read_number_or(fields, "padding-px", ctx, 2.0)? as f32;
        let (sort_key_expr, sort_key_expr_src) =
            parse_expr_field(fields, "sort-key-expr", &maplibre_expr::Type::Number)?;
        // Neighbour candidate gathering: the upstream `<source>.<layer>`
        // (used only to name the neighbour bindings in `asset_inputs`) plus
        // the upstream feature filter, so neighbours are filtered exactly
        // like the tile's own features. All optional — absent → the node
        // collides against its own tile's features only.
        let source = read_optional_string(fields, "source")?;
        let layer = read_optional_string(fields, "layer")?;
        let neighbor_base = match (source, layer) {
            (Some(s), Some(l)) => Some(format!("{s}.{l}")),
            _ => None,
        };
        let (filter_expr, filter_expr_src) = match fields.get("filter-expr") {
            Some(v) => {
                let expr = maplibre_expr::parse(v).map_err(|e| FactoryError::BadField {
                    field: "filter-expr".into(),
                    msg: e.to_string(),
                })?;
                (Some(expr), Some(v.to_string()))
            }
            None => (None, None),
        };
        let min_zoom_field = read_optional_string(fields, "min-zoom-field")?;

        let mut ports = vec![PortSpec {
            name: "features",
            accepts: &[PortKind::Features],
            optional: false,
        }];
        ports.extend(parts.ports);
        let mut connections = vec![Connection {
            port: "features".into(),
            src: features,
        }];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(TextNode {
                font_keys,
                text,
                text_expr,
                size,
                color,
                halo_color,
                halo_width,
                opacity,
                size_expr,
                color_expr,
                halo_color_expr,
                halo_width_expr,
                opacity_expr,
                text_expr_src,
                size_expr_src,
                color_expr_src,
                halo_color_expr_src,
                halo_width_expr_src,
                opacity_expr_src,
                anchor,
                justify,
                transform,
                offset_em,
                max_width_em,
                line_height,
                letter_spacing_em,
                max_extent_px,
                collide,
                allow_overlap,
                ignore_placement,
                padding_px,
                sort_key_expr,
                sort_key_expr_src,
                neighbor_base,
                filter_expr,
                filter_expr_src,
                min_zoom_field,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Text labels at feature points (MapLibre point placement). `font` is an ordered fallback stack of `font` and/or `glyphs` source names; `text` is a literal string or a MapLibre string expression evaluated per feature group. Paint properties have optional `*-expr` siblings; layout knobs are build-time constants in em. Collision is on by default and is deterministic across tiles: candidates come from this tile plus the 8 neighbour tiles (host-bound under `<source>.<layer>@dx,dy`), so borders stay seamless. Set `source`/`layer` (the upstream feature source) to enable neighbour gathering; without them collision is centre-tile-only.",
            "properties": {
                "features": schema_frag::node_ref(),
                "font": { "type": "array", "items": { "type": "string" },
                          "description": "Ordered fallback stack of `font` and/or `glyphs` source names from the document's `sources`. Outline fonts and SDF glyph stacks mix freely; the first entry covering a char shapes it." },
                "text": {
                    "description": "The label: a literal string, or a MapLibre string expression (evaluated per feature group; empty/failed → the group draws nothing).",
                },
                "size": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0,
                          "description": "Font size in px. Default 16." })),
                "size-expr": {
                    "description": "A MapLibre number expression (px), evaluated per feature group; overrides the constant `size`.",
                },
                "color": schema_frag::color(),
                "color-expr": {
                    "description": "A MapLibre color expression, evaluated per feature group; overrides the constant `color`.",
                },
                "halo-color": schema_frag::color(),
                "halo-color-expr": {
                    "description": "A MapLibre color expression for the halo, evaluated per feature group; overrides the constant `halo-color`.",
                },
                "halo-width": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0,
                                "description": "Halo radius in px around each glyph. Default 0 (no halo)." })),
                "halo-width-expr": {
                    "description": "A MapLibre number expression (px), evaluated per feature group; overrides the constant `halo-width`.",
                },
                "opacity": schema_frag::unit_number(),
                "opacity-expr": {
                    "description": "A MapLibre number expression giving opacity, evaluated per feature group; multiplies both fill and halo alpha. Overrides the constant `opacity`.",
                },
                "anchor": { "type": "string", "enum": ["center", "left", "right", "top", "bottom", "top-left", "top-right", "bottom-left", "bottom-right"],
                            "description": "Which part of the label block sits on the point. Default `center`." },
                "offset-em": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2,
                               "description": "Block shift [x, y] in em, applied after anchoring. Default [0, 0]." },
                "justify": { "type": "string", "enum": ["auto", "left", "center", "right"],
                             "description": "Line alignment within the wrapped block. Default `auto` (follows the anchor's horizontal side)." },
                "max-width-em": { "type": "number", "minimum": 0.0,
                                  "description": "Wrap target width in em. Default 10; 0 disables wrapping." },
                "line-height": { "type": "number",
                                 "description": "Baseline-to-baseline distance in em. Default 1.2." },
                "letter-spacing-em": { "type": "number",
                                       "description": "Extra advance per glyph in em. Default 0." },
                "transform": { "type": "string", "enum": ["none", "uppercase", "lowercase"],
                               "description": "Case transform applied before shaping. Default `none`." },
                "max-extent-px": { "type": "number", "minimum": 0.0,
                                   "description": "Canvas pad this node requests; labels whose bbox half-extent exceeds it are culled with a warning. Default 128." },
                "collide": { "type": "boolean",
                             "description": "Whether to run deterministic label collision. Default true (MapLibre's default). Set false to draw every label (the pre-collision behaviour)." },
                "allow-overlap": { "type": "boolean",
                                   "description": "MapLibre `text-allow-overlap`: place the label even if it collides. It still reserves its box (blocking later labels) unless `ignore-placement`. Default false." },
                "ignore-placement": { "type": "boolean",
                                      "description": "MapLibre `text-ignore-placement`: don't let this label block later ones (skip inserting its collision box). Default false." },
                "padding-px": { "type": "number", "minimum": 0.0,
                                "description": "Collision-box inflation in px on every side. Default 2." },
                "sort-key-expr": {
                    "description": "A MapLibre number expression (MapLibre `symbol-sort-key`), evaluated per feature group; lower values place first under collision. Absent = 0.",
                },
                "source": { "type": "string",
                            "description": "Upstream feature source name (matches the `features` node). Used only to name the neighbour tile bindings for cross-tile collision. Omit to collide within this tile only." },
                "layer": { "type": "string",
                           "description": "Upstream feature layer name (with `source`). Used only to name neighbour bindings for cross-tile collision." },
                "filter-expr": {
                    "description": "The upstream `features` filter, reproduced when gathering neighbour candidates so they are filtered identically to this tile's own features. Set it to whatever the `features` node uses.",
                },
                "min-zoom-field": { "type": "string",
                                    "description": "Per-feature `min_zoom` property name, reproduced for neighbour candidate filtering (mirrors the `features` node)." },
            },
            "required": ["features", "font", "text"],
        })
    }
}

ezu_graph::submit_node!(TextFactory);
