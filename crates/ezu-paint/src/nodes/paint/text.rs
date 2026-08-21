//! `text` — `Features -> Raster`. Draw text labels, shaped and laid out
//! by `ezu-core`'s `text` module, with deterministic cross-tile
//! collision, placing its own candidates.
//!
//! `text-labels` — `Features -> Labels`. The same node with the same
//! fields, stopping at the candidates so a shared `label-placement` node
//! can decide them against every other label layer's, the way MapLibre
//! collides all symbol layers against one index. A recipe with more than
//! one label layer wants this (see [`super::labels`]); a lone layer gets
//! the same result either way.
//!
//! `placement` picks the MapLibre `symbol-placement` mode:
//! `point` (default) labels each feature point; `line` / `line-center`
//! walk each polyline with tangent-rotated glyphs (see
//! [`ezu_core::text::line`]).
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
//! Point placement ignores lines/polygons; line placement ignores
//! points/polygons. Drawing is a pure function of world position (no
//! jitter), so labels match across tile borders. Collision (default on)
//! is likewise world-space deterministic: candidates are gathered from
//! this tile plus its 8 neighbour tiles (host-bound under
//! `<source>.<layer>@dx,dy`), deduped by `(layer, text, quantized world
//! anchor)`, ordered by `(layer, sort-key, world anchor, text)`, and placed
//! greedily against a grid — all from world-space quantities, so
//! adjacent tiles agree on every straddling label (a line label
//! collides per glyph, all-or-nothing). Missing neighbour bindings
//! degrade to centre-only. MVT clips lines per tile, so a clipped
//! line's arc-lengths — and thus its `line` spacing anchors — can
//! differ slightly between the tiles that see it; the quantized dedup
//! absorbs the common case, but anchors near a clip edge may diverge
//! (MapLibre regenerates per tile and reconciles with tolerance
//! matching instead). Divergence from MapLibre is deliberate and not
//! emulated: no viewport-centre priority and no per-frame fade in/out.
//! See [`ezu_core::text::collide`].

use std::collections::HashMap;
use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, Asset, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError,
    FactoryCtx, FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    downcast_features, read_bool_or, read_number_or, read_optional_string, read_optional_zoom,
    read_string_or, read_xy, FeatureGroup,
};
use crate::render::{collect_groups, SharedLayer};
use ezu_core::text::{
    clip_line,
    collide::{self, Aabb, LabelCandidate, PlaceRank},
    generate_anchors, get_or_build_layout, layout_sections, place_glyphs, Anchor, AnchorParams,
    FaceEntry, Font, GlyphPlacement, Justify, LayoutParams, LinePlacement, SdfFontStack,
    SectionPaint, SectionSpec, StackEntry, TextBlock, TextPaint, TextTransform, VerticalAlign,
};

use super::icon_fit::{fitted_content_box, stretch_image, IconTextFit, NineSlice};
use super::labels::{draw_labels, set_id, FaceCache, IconDraw, LabelDraw, LabelSet, PointVariant};

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

/// Emit a debug summary of the process-wide shaped-layout cache activity for
/// one eval: blocks laid out fresh (misses) vs served from the shared cache
/// (hits). Nothing is logged when no outline-stack block was keyed.
fn log_layout_cache_stats(hits: usize, misses: usize) {
    if hits > 0 || misses > 0 {
        tracing::debug!(
            built = misses,
            hits,
            "text: shaped layouts served from the shared cache"
        );
    }
}

/// The process-wide shaped-layout cache key for one laid-out block: a hash of
/// every input reaching [`layout_sections`] — the flat font stack by each
/// font's stable content hash, the section specs (text, `font-scale`,
/// `vertical-align`, and font subrange), the font size, and every
/// [`LayoutParams`] field that shifts a glyph or the bounding box.
///
/// Returns `None` when any stack entry is an SDF glyph stack: an SDF stack's
/// coverage is mutable (ranges are fetched lazily), so its layout is not
/// safely content-addressable across evals — such a label falls back to the
/// per-eval cache only. The pm basemap stacks are all outline fonts, so this
/// keys every block there.
fn layout_cache_key(
    flat_fonts: &[StackEntry],
    sections: &[LabelSection],
    ranges: &[std::ops::Range<usize>],
    size: f32,
    params: &LayoutParams,
) -> Option<u64> {
    let mut h = Xxh3::new();
    for entry in flat_fonts {
        match entry {
            StackEntry::Outline(font) => h.update(&font.content_hash().to_le_bytes()),
            StackEntry::Sdf(_) => return None,
        }
    }
    // Separate the font list from the section list so a font hash can't be
    // confused with a range index.
    h.update(&[0xff]);
    for (s, r) in sections.iter().zip(ranges) {
        h.update(s.text.as_bytes());
        h.update(&[0]);
        h.update(&s.scale.to_bits().to_le_bytes());
        h.update(&[s.valign as u8]);
        h.update(&(r.start as u32).to_le_bytes());
        h.update(&(r.end as u32).to_le_bytes());
    }
    h.update(&size.to_bits().to_le_bytes());
    h.update(&params.max_width_em.to_bits().to_le_bytes());
    h.update(&params.line_height_em.to_bits().to_le_bytes());
    h.update(&params.letter_spacing_em.to_bits().to_le_bytes());
    h.update(&[
        params.anchor as u8,
        params.justify as u8,
        params.transform as u8,
    ]);
    h.update(&params.offset_em[0].to_bits().to_le_bytes());
    h.update(&params.offset_em[1].to_bits().to_le_bytes());
    Some(h.digest())
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

/// The gap (em) MapLibre's shaping leaves between a block's box edge and the
/// glyph baseline, subtracted from a vertically anchored offset so the visible
/// text keeps the requested distance from the point.
const BASELINE_OFFSET_EM: f32 = 7.0 / 24.0;

/// The block shift (em, y-down) an anchor applies — MapLibre's
/// `evaluateVariableOffset`.
///
/// With `text-radial-offset` the label is pushed `r` em away from the point in
/// the anchor's direction (`top` → below the point, `left` → right of it), and
/// corners split the distance along both axes. Otherwise `text-offset` is
/// **mirrored**: its magnitude points away from the anchored edge, so a `right`
/// anchor shifts left by `|dx|` where a `left` anchor shifts right by it, and
/// an axis the anchor doesn't name contributes nothing.
fn anchor_offset_em(anchor: Anchor, offset: [f32; 2], radial: f32) -> [f32; 2] {
    if radial > 0.0 {
        let diag = radial / std::f32::consts::SQRT_2;
        let b = BASELINE_OFFSET_EM;
        let (x, y) = match anchor {
            Anchor::Center => (0.0, 0.0),
            Anchor::Left => (radial, 0.0),
            Anchor::Right => (-radial, 0.0),
            Anchor::Top => (0.0, radial - b),
            Anchor::Bottom => (0.0, -radial + b),
            Anchor::TopLeft => (diag, diag - b),
            Anchor::TopRight => (-diag, diag - b),
            Anchor::BottomLeft => (diag, -diag + b),
            Anchor::BottomRight => (-diag, -diag + b),
        };
        return [x, y];
    }
    let (ox, oy) = (offset[0].abs(), offset[1].abs());
    let b = BASELINE_OFFSET_EM;
    let x = match anchor {
        Anchor::Left | Anchor::TopLeft | Anchor::BottomLeft => ox,
        Anchor::Right | Anchor::TopRight | Anchor::BottomRight => -ox,
        _ => 0.0,
    };
    let y = match anchor {
        Anchor::Top | Anchor::TopLeft | Anchor::TopRight => oy - b,
        Anchor::Bottom | Anchor::BottomLeft | Anchor::BottomRight => -oy + b,
        _ => 0.0,
    };
    [x, y]
}

/// Neighbour prefilter band width, in multiples of the node's widest label
/// reach. A label reaches at most one reach from its anchor, so a chain of
/// colliding labels advances one reach per hop; this many hops is the depth at
/// which a dropped neighbour can no longer perturb a label the tile draws. Wide
/// enough that filtering leaves the render bit-identical to gathering every
/// neighbour; a tighter band starts to disturb dense collision chains.
const NEIGHBOR_BAND_HOPS: f32 = 3.0;

/// Arc window (in em) the `text-max-angle` bend is summed over for line
/// placement — MapLibre's 3/5 of the font size.
const ANGLE_WINDOW_EM: f32 = 0.6;

/// Whether a neighbour feature's geometry — its bounding box in the local
/// world-pixel frame — comes within `band` px of this tile's rectangle
/// `[0, tile_w] × [0, tile_h]`. Only such features can host a label (or sit
/// in a collision chain) that changes what this tile draws; the rest are
/// dropped before shaping.
fn bbox_within_band(
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
    tile_w: f32,
    tile_h: f32,
    band: f32,
) -> bool {
    max_x + band >= 0.0 && min_x - band <= tile_w && max_y + band >= 0.0 && min_y - band <= tile_h
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

/// Assemble label sections into a flat font stack, per-section font subranges
/// (aligned with `sections`), and a content hash. Distinct `font_id`s appear
/// once in the flat stack in first-use order, so a glyph's flat font index and
/// the layout are a deterministic function of the sections — the hash keys the
/// layout cache and is reused as the collision `style_id`. The hash folds each
/// section's text, `font_id`, and scale (colour is draw-only, excluded).
fn assemble_stacks(
    sections: &[LabelSection],
    registry: &FontRegistry,
) -> (Vec<StackEntry>, Vec<std::ops::Range<usize>>, u64) {
    let mut flat: Vec<StackEntry> = Vec::new();
    let mut by_id: HashMap<u32, std::ops::Range<usize>> = HashMap::new();
    let mut ranges = Vec::with_capacity(sections.len());
    let mut h = Xxh3::new();
    for s in sections {
        let range = match by_id.get(&s.font_id) {
            Some(r) => r.clone(),
            None => {
                let start = flat.len();
                flat.extend_from_slice(registry.stack_by_id(s.font_id));
                let r = start..flat.len();
                by_id.insert(s.font_id, r.clone());
                r
            }
        };
        ranges.push(range);
        h.update(s.text.as_bytes());
        h.update(&[0]);
        h.update(&s.font_id.to_le_bytes());
        h.update(&s.scale.to_bits().to_le_bytes());
    }
    (flat, ranges, h.digest())
}

/// The per-section fill table for [`draw`], with the group `opacity` folded
/// into each override's alpha. Sections with no colour override carry `None`
/// (the glyph then uses the block fill).
fn section_paints(sections: &[LabelSection], opacity: f32) -> Vec<SectionPaint> {
    sections
        .iter()
        .map(|s| SectionPaint {
            color: s.color.map(|[r, g, b, a]| [r, g, b, a * opacity]),
        })
        .collect()
}

/// Whitespace MapLibre trims at the ends of a laid-out line (mirrors the
/// private set in `ezu_core::text::layout`).
fn is_layout_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ')
}

/// A safe lower bound (em) on the shaped width of a single-line label, computed
/// from nominal font advances without shaping. Each covered char contributes
/// its first-covering font's cmap+hmtx advance times its section `font-scale`;
/// an uncovered char contributes 0 (it may fall back to a wider font, so 0 is
/// the safe side). Leading and trailing whitespace is trimmed to match the
/// single-line layout, while interior whitespace counts as it does there.
///
/// The nominal advance ignores kerning and ligatures — both only shrink the
/// shaped width — so the sum over-estimates it; `SHRINK` scales it back under
/// the true width. Negative letter spacing (which the layout adds per glyph,
/// and glyphs never outnumber chars) is charged per char so the bound stays
/// below the shaped width. The result is `≤ block.bbox.width()` for every
/// real label (asserted while validating `SHRINK`).
fn line_lower_bound_width_em(
    sections: &[LabelSection],
    ranges: &[std::ops::Range<usize>],
    view: &[FaceEntry<'_>],
    transform: TextTransform,
    letter_spacing_em: f32,
) -> f32 {
    /// Fraction of the nominal advance sum kept as the lower bound; the
    /// headroom absorbs kerning / ligature shrink.
    const SHRINK: f32 = 0.8;
    // Per-char nominal advances (em, section-scaled) plus a whitespace flag,
    // in layout order, so leading/trailing whitespace can be trimmed.
    let mut advances: Vec<(f32, bool)> = Vec::new();
    for (s, r) in sections.iter().zip(ranges) {
        let sub = &view[r.clone()];
        let transformed = match transform {
            TextTransform::None => s.text.clone(),
            TextTransform::Uppercase => s.text.to_uppercase(),
            TextTransform::Lowercase => s.text.to_lowercase(),
        };
        for c in transformed.chars() {
            let adv = sub.iter().find_map(|fe| fe.advance_em(c)).unwrap_or(0.0) * s.scale;
            advances.push((adv, is_layout_whitespace(c)));
        }
    }
    let first = advances.iter().position(|&(_, ws)| !ws);
    let last = advances.iter().rposition(|&(_, ws)| !ws);
    let (Some(first), Some(last)) = (first, last) else {
        return 0.0;
    };
    let trimmed = &advances[first..=last];
    let sum: f32 = trimmed.iter().map(|&(a, _)| a).sum();
    let ls_penalty = letter_spacing_em.min(0.0) * trimmed.len() as f32;
    SHRINK * sum + ls_penalty
}

/// Canonical registry key for a font stack: names trimmed, joined with `,`
/// (no space). Matches `maplibre_expr`'s `FormatSection::font_stack` so a
/// `format` section's stack and a `font-expr` result look up identically.
fn stack_key<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names.map(str::trim).collect::<Vec<_>>().join(",")
}

/// All of a `text` node's font stacks, loaded once per eval. `font_id` is a
/// stable small integer — `0` = the default stack, `i + 1` = the i-th
/// registry entry in recipe declaration order — so two tiles built from the
/// same recipe assign identical ids (used by the layout cache and the
/// collision tie-break).
struct FontRegistry {
    /// `font_id` 0.
    default: Vec<StackEntry>,
    /// `font_id` i+1, aligned with `TextNode::font_stacks`.
    stacks: Vec<Vec<StackEntry>>,
    /// Canonical stack key → index into `stacks`.
    by_key: HashMap<String, usize>,
}

impl FontRegistry {
    /// Resolve a stack key to `(font_id, stack)`. `None`, an eval miss, or an
    /// unregistered key → the default stack (`font_id` 0).
    fn resolve(&self, key: Option<&str>) -> (u32, &[StackEntry]) {
        match key.and_then(|k| self.by_key.get(k)) {
            Some(&i) => (i as u32 + 1, &self.stacks[i]),
            None => (0, &self.default),
        }
    }

    /// The stack for a resolved `font_id` (`0` = default).
    fn stack_by_id(&self, id: u32) -> &[StackEntry] {
        if id == 0 {
            &self.default
        } else {
            &self.stacks[(id - 1) as usize]
        }
    }

    /// The `font_id` for a canonical stack key, or `None` if unregistered.
    fn id_of(&self, key: &str) -> Option<u32> {
        self.by_key.get(key).map(|&i| i as u32 + 1)
    }

    /// Every stack in the registry — the input to the eval's [`FaceCache`].
    fn all_stacks(&self) -> impl Iterator<Item = &[StackEntry]> {
        std::iter::once(self.default.as_slice()).chain(self.stacks.iter().map(Vec::as_slice))
    }
}

/// One resolved `format` section (or a plain label as a single section):
/// its text, the registry `font_id` for its stack, its `font-scale`, and an
/// optional per-section fill color (straight sRGB, group opacity not yet
/// applied).
struct LabelSection {
    text: String,
    font_id: u32,
    scale: f32,
    color: Option<[f32; 4]>,
    valign: VerticalAlign,
}

/// A feature group evaluated once and ready to shape: its label sections and
/// per-group paint/layout scalars (group opacity already folded into the fill
/// and halo alpha), plus the source group and its neighbour offset. Built by
/// [`TextNode::prep_group`] so the neighbour prefilter can size its band from
/// `reach` — the widest a label can lay out — without a second evaluation pass.
struct GroupPrep<'g> {
    group: &'g FeatureGroup,
    dx: i64,
    dy: i64,
    /// The group's position in its own tile's filtered layer — MapLibre's
    /// within-layer placement order.
    feature: u32,
    sections: Vec<LabelSection>,
    /// The sections concatenated, as they lay out — the collision dedup text.
    /// An icon-only symbol carries its icon name here instead, so two
    /// different icons on one spot stay distinct labels.
    text: String,
    /// The group's resolved `icon-image`, placed together with the text.
    icon: Option<GroupIcon>,
    size: f32,
    sort_key: f64,
    opacity: f32,
    padding: f32,
    color: [f32; 4],
    halo_color: [f32; 4],
    halo_width: f32,
    reach: f32,
    /// A `font-expr` was set but resolved to the default stack (warned, own only).
    font_fallback: bool,
}

/// Resolve an evaluated `text` value into label sections, or `None` to skip
/// the label (null/other types, or a `format` of only images/empty text).
///
/// A plain string / number / bool becomes a single section on `base_font_id`
/// (the group's `font-expr`-resolved stack). A `format` value becomes one
/// section per styled span: its `font-scale`, its per-section `text-color`,
/// and its `text-font` resolved against the registry (an unlisted or absent
/// stack falls back to `base_font_id`). Image sections are skipped.
fn label_sections(
    value: &maplibre_expr::Value,
    registry: &FontRegistry,
    base_font_id: u32,
) -> Option<Vec<LabelSection>> {
    use maplibre_expr::Value as V;
    let one = |text: String| {
        Some(vec![LabelSection {
            text,
            font_id: base_font_id,
            scale: 1.0,
            color: None,
            valign: VerticalAlign::Baseline,
        }])
    };
    match value {
        V::String(s) => one(s.clone()),
        V::Number(n) => one(n.to_string()),
        V::Bool(b) => one(b.to_string()),
        V::Formatted(secs) => {
            let sections: Vec<LabelSection> = secs
                .iter()
                .filter(|s| s.image.is_none() && !s.text.is_empty())
                .map(|s| LabelSection {
                    text: s.text.clone(),
                    font_id: s
                        .font_stack
                        .as_deref()
                        .and_then(|k| registry.id_of(k))
                        .unwrap_or(base_font_id),
                    scale: s.scale.map(|v| v as f32).unwrap_or(1.0),
                    color: s
                        .text_color
                        .as_ref()
                        .map(|c| [c.r as f32, c.g as f32, c.b as f32, c.a as f32]),
                    valign: s
                        .vertical_align
                        .as_deref()
                        .and_then(VerticalAlign::parse)
                        .unwrap_or_default(),
                })
                .collect();
            (!sections.is_empty()).then_some(sections)
        }
        _ => None,
    }
}

/// MapLibre `symbol-placement`: where the label sits relative to its
/// feature. Line modes consume the feature's polylines; point mode its
/// points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    Point,
    Line,
    LineCenter,
}

impl Placement {
    fn parse(s: &str) -> Option<Placement> {
        Some(match s {
            "point" => Placement::Point,
            "line" => Placement::Line,
            "line-center" => Placement::LineCenter,
            _ => return None,
        })
    }
    /// The `ezu_core` line mode, or `None` for point placement.
    fn line(self) -> Option<LinePlacement> {
        match self {
            Placement::Point => None,
            Placement::Line => Some(LinePlacement::Line),
            Placement::LineCenter => Some(LinePlacement::LineCenter),
        }
    }
}

/// The `icon-image` half of a symbol: which sprite to crop, how to name the
/// per-feature icon, and MapLibre's icon layout / paint knobs. A symbol's
/// icon and text are placed as one unit (see [`symbol_variants`]), so the
/// icon lives on the label node rather than on a separate `stamp`.
struct IconConfig {
    /// Sprite atlas asset key (the `sprite` source's `image`).
    sprite: String,
    /// Constant `icon-image`; `None` when `name_expr` names it per feature.
    name: Option<String>,
    name_expr: Option<maplibre_expr::Expr>,
    name_expr_src: Option<String>,
    /// MapLibre `icon-size`: multiplier on the sprite's display size.
    size: f32,
    size_expr: Option<maplibre_expr::Expr>,
    size_expr_src: Option<String>,
    /// MapLibre `icon-rotate`, degrees clockwise.
    rotate_deg: f32,
    rotate_expr: Option<maplibre_expr::Expr>,
    rotate_expr_src: Option<String>,
    opacity: f32,
    opacity_expr: Option<maplibre_expr::Expr>,
    opacity_expr_src: Option<String>,
    /// MapLibre `icon-anchor`: which part of the image sits on the point.
    anchor: Anchor,
    /// MapLibre `icon-offset` in px, itself scaled by `icon-size`.
    offset: [f32; 2],
    /// MapLibre `icon-padding`: collision-box inflation in px.
    padding_px: f32,
    padding_expr: Option<maplibre_expr::Expr>,
    padding_expr_src: Option<String>,
    allow_overlap: bool,
    ignore_placement: bool,
    /// MapLibre `icon-optional`: the text may place without the icon.
    optional: bool,
    /// MapLibre `icon-text-fit`: which axes stretch to the placed label.
    text_fit: IconTextFit,
    /// MapLibre `icon-text-fit-padding`, `[top, right, bottom, left]` px.
    text_fit_padding: [f32; 4],
}

/// A symbol's icon at its sprite size, anchored by `icon-anchor` — the
/// no-`icon-text-fit` case. Returns the draw payload and the collision box it
/// occupies relative to the symbol anchor.
fn plain_icon(
    cfg: &IconConfig,
    gi: &GroupIcon,
    image: Arc<ezu_graph::RasterBuf>,
    scale: f32,
) -> (Arc<IconDraw>, Aabb) {
    let (w, h) = (image.width as f32 * scale, image.height as f32 * scale);
    let (fx, fy) = cfg.anchor.fraction();
    let offset = (
        (0.5 - fx) * w + cfg.offset[0] * gi.size,
        (0.5 - fy) * h + cfg.offset[1] * gi.size,
    );
    // The collision box is the unrotated rect, as MapLibre's point-placed
    // icon box is; a rotated icon only reaches further on the canvas, which
    // `half` covers for the off-canvas reject.
    let rel = Aabb {
        min_x: offset.0 - 0.5 * w,
        min_y: offset.1 - 0.5 * h,
        max_x: offset.0 + 0.5 * w,
        max_y: offset.1 + 0.5 * h,
    }
    .inflate(gi.padding);
    let half = if gi.rotate_deg == 0.0 {
        (0.5 * w, 0.5 * h)
    } else {
        let d = 0.5 * (w * w + h * h).sqrt();
        (d, d)
    };
    (
        Arc::new(IconDraw {
            image,
            offset,
            scale,
            rotation_deg: gi.rotate_deg,
            opacity: gi.opacity,
            half,
        }),
        rel,
    )
}

/// A symbol's icon stretched to the box `block` occupies (MapLibre
/// `icon-text-fit`). The sprite's nine-slice bands absorb the growth, so the
/// stretched image is pre-composed here and drawn at 1:1; the collision box is
/// the fitted content box, which is what the label actually reserves.
#[allow(clippy::too_many_arguments)]
fn fitted_icon(
    cfg: &IconConfig,
    gi: &GroupIcon,
    image: &Arc<ezu_graph::RasterBuf>,
    rect: Option<&ezu_graph::SpriteRect>,
    scale: f32,
    block: &TextBlock,
    size: f32,
) -> (Arc<IconDraw>, Aabb) {
    let bb = block.bbox;
    let text = Aabb {
        min_x: bb.min_x * size,
        min_y: bb.min_y * size,
        max_x: bb.max_x * size,
        max_y: bb.max_y * size,
    };
    let display = (image.width as f32 * scale, image.height as f32 * scale);
    let fitted = fitted_content_box(
        cfg.text_fit,
        text,
        display,
        cfg.text_fit_padding,
        (cfg.offset[0] * gi.size, cfg.offset[1] * gi.size),
    );
    let slice = NineSlice {
        stretch_x: rect.map_or(&[][..], |r| &r.stretch_x),
        stretch_y: rect.map_or(&[][..], |r| &r.stretch_y),
        content: rect.and_then(|r| r.content),
    };
    let target = (fitted.max_x - fitted.min_x, fitted.max_y - fitted.min_y);
    let Some((stretched, origin)) = stretch_image(image, &slice, target, scale) else {
        return plain_icon(cfg, gi, image.clone(), scale);
    };
    let (w, h) = (stretched.width as f32, stretched.height as f32);
    // The drawn image's top-left sits `origin` from the fitted box's, so its
    // centre lands here relative to the symbol anchor.
    let offset = (
        fitted.min_x + origin.0 + 0.5 * w,
        fitted.min_y + origin.1 + 0.5 * h,
    );
    let half = if gi.rotate_deg == 0.0 {
        (0.5 * w, 0.5 * h)
    } else {
        let d = 0.5 * (w * w + h * h).sqrt();
        (d, d)
    };
    (
        Arc::new(IconDraw {
            image: Arc::new(stretched),
            offset,
            scale: 1.0,
            rotation_deg: gi.rotate_deg,
            opacity: gi.opacity,
            half,
        }),
        fitted.inflate(gi.padding),
    )
}

/// One feature group's resolved icon: the named image plus the per-group
/// scalars. `None` for a group whose `icon-image` is absent or empty — such
/// a symbol is text-only, exactly as in MapLibre.
struct GroupIcon {
    name: String,
    size: f32,
    rotate_deg: f32,
    opacity: f32,
    padding: f32,
}

impl IconConfig {
    /// Resolve this config for one feature group, or `None` when the group
    /// names no icon (empty / non-string result, or a zero size).
    fn eval_group(&self, ectx: &maplibre_expr::EvaluationContext) -> Option<GroupIcon> {
        let name = match &self.name_expr {
            Some(e) => match maplibre_expr::evaluate(e, ectx) {
                Ok(maplibre_expr::Value::String(s)) => s,
                _ => return None,
            },
            None => self.name.clone()?,
        };
        if name.is_empty() {
            return None;
        }
        let size = eval_number(&self.size_expr, ectx, self.size).max(0.0);
        if size <= 0.0 {
            return None;
        }
        Some(GroupIcon {
            name,
            size,
            rotate_deg: eval_number(&self.rotate_expr, ectx, self.rotate_deg),
            opacity: eval_number(&self.opacity_expr, ectx, self.opacity).clamp(0.0, 1.0),
            padding: eval_number(&self.padding_expr, ectx, self.padding_px).max(0.0),
        })
    }
}

/// One symbol's collision variants: the box sets to try, and what each one
/// draws. MapLibre places a symbol's icon and text together — both boxes
/// must be free or the whole symbol drops — and the `*-optional` flags admit
/// the fallbacks that keep just one of the two. Trying `[icon, text]` first
/// and the permitted single-box sets after reproduces that, since a variant
/// places only when every box in it is free and reserves exactly the boxes
/// it placed.
///
/// `text_boxes` holds one box per anchor candidate (empty for an icon-only
/// symbol); `icon_boxes` holds the icon's box — one shared by every anchor,
/// or one per anchor when `icon-text-fit` sizes the icon to the label that
/// anchor lays out. Empty means the symbol has no icon.
fn symbol_variants(
    text_boxes: &[Aabb],
    icon_boxes: &[Aabb],
    text_optional: bool,
    icon_optional: bool,
) -> (Vec<Vec<Aabb>>, Vec<PointVariant>) {
    let mut boxes = Vec::new();
    let mut variants = Vec::new();
    // A single icon box serves every anchor; a fitted one is picked per anchor.
    let icon_at = |ix: usize| icon_boxes[ix.min(icon_boxes.len() - 1)];
    if icon_boxes.is_empty() {
        for (ix, b) in text_boxes.iter().enumerate() {
            boxes.push(vec![*b]);
            variants.push(PointVariant {
                block: Some(ix),
                icon: None,
            });
        }
    } else if text_boxes.is_empty() {
        boxes.push(vec![icon_at(0)]);
        variants.push(PointVariant {
            block: None,
            icon: Some(0),
        });
    } else {
        for (ix, b) in text_boxes.iter().enumerate() {
            boxes.push(vec![icon_at(ix), *b]);
            variants.push(PointVariant {
                block: Some(ix),
                icon: Some(ix.min(icon_boxes.len() - 1)),
            });
        }
        if text_optional {
            boxes.push(vec![icon_at(0)]);
            variants.push(PointVariant {
                block: None,
                icon: Some(0),
            });
        }
        if icon_optional {
            for (ix, b) in text_boxes.iter().enumerate() {
                boxes.push(vec![*b]);
                variants.push(PointVariant {
                    block: Some(ix),
                    icon: None,
                });
            }
        }
    }
    (boxes, variants)
}

/// Which half of the label pipeline a [`TextNode`] runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// `text`: shape, place against this layer's own candidates, and draw.
    Whole,
    /// `text-labels`: shape into candidates and emit them for a shared
    /// `label-placement` node to decide.
    Labels,
}

/// Default `max-extent-px`: the canvas pad a label may need to cross a tile
/// border un-clipped. Shared with `text-draw`, which requests the same pad.
pub(super) const DEFAULT_MAX_EXTENT_PX: f32 = 128.0;

struct TextNode {
    /// Whether this node draws its labels itself or hands its candidates to
    /// a shared `label-placement` node.
    stage: Stage,
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
    /// (A) Data-driven `text-font`: a MapLibre expression yielding an array
    /// of font names, evaluated per feature group. Its result is canonicalized
    /// (see [`stack_key`]) and looked up in `font_stacks`; a miss falls back to
    /// the default stack. `None` → the static default stack for every feature.
    font_expr: Option<maplibre_expr::Expr>,
    font_expr_src: Option<String>,
    /// Build-resolved registry: `(canonical stack key, asset keys)` in recipe
    /// declaration order. Entry index + 1 is the stack's stable `font_id`.
    font_stacks: Vec<(String, Vec<String>)>,
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
    /// MapLibre `icon-image` and its layout / paint knobs. Point placement
    /// only: a line-placed icon has no ezu equivalent, so line labels ignore
    /// it. The icon shares its symbol's collision decision with the text.
    icon: Option<IconConfig>,
    /// MapLibre `text-optional`: the icon may place without the text.
    text_optional: bool,
    /// MapLibre `symbol-placement`. Point placement uses each group's
    /// points; line / line-center placement walks its polylines.
    placement: Placement,
    /// MapLibre `symbol-spacing`: gap (px) between successive line
    /// anchors. Line placement only.
    spacing_px: f32,
    /// MapLibre `text-max-angle`: max cumulative bend (degrees) allowed
    /// over a label-length window. Line placement only.
    max_angle_deg: f32,
    /// MapLibre `text-keep-upright`: flip a right-to-left label so it
    /// reads upright. Line placement only.
    keep_upright: bool,
    /// Build-time layout constants (em units except where noted).
    anchor: Anchor,
    /// MapLibre `text-variable-anchor`: ordered anchor candidates tried on
    /// collision, the first free one placed. Empty → the fixed `anchor`.
    anchor_variants: Vec<Anchor>,
    /// MapLibre `text-radial-offset`: distance in em each variable anchor
    /// pushes the block away from the point, in the anchor's direction.
    radial_offset_em: f32,
    justify: Justify,
    transform: TextTransform,
    offset_em: [f32; 2],
    max_width_em: f32,
    line_height: f32,
    letter_spacing_em: f32,
    /// Labels whose rendered bbox half-extent exceeds this many px are
    /// culled (they'd overflow the canvas pad this node requested).
    max_extent_px: f32,
    /// Render outline-font glyphs through the SDF field-sampling path
    /// (maplibre-gl-js style) rather than a per-glyph vector fill / stroke.
    /// SDF glyph stacks are unaffected — they always sample the field.
    outline_sdf: bool,
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
    /// Data-driven `text-padding`: a MapLibre number expression evaluated per
    /// feature group; overrides the constant `padding_px` for that group.
    padding_expr: Option<maplibre_expr::Expr>,
    padding_expr_src: Option<String>,
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
    /// The style layer's zoom band, mirroring the upstream `features` node.
    /// Neighbour candidates are gathered from the source directly, so the
    /// node has to honour the gate itself or a layer that is off at this
    /// zoom would still spill its neighbours' labels across the seam.
    min_zoom: Option<u8>,
    max_zoom: Option<u8>,
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

    /// Layout params for one anchor candidate: the block is re-anchored and
    /// shifted away from the point in that anchor's direction. A layer with
    /// `text-variable-anchor` (or any `text-radial-offset`) re-evaluates its
    /// offset per anchor, so the label always sits on the far side of the
    /// point; a plain fixed anchor takes `text-offset` as written.
    fn variant_layout_params(&self, anchor: Anchor) -> LayoutParams {
        let offset_em = if self.anchor_variants.is_empty() && self.radial_offset_em <= 0.0 {
            self.offset_em
        } else {
            anchor_offset_em(anchor, self.offset_em, self.radial_offset_em)
        };
        LayoutParams {
            anchor,
            offset_em,
            ..self.layout_params()
        }
    }

    /// The anchors to try for a label: the `text-variable-anchor` list, or the
    /// single fixed `anchor` when none is set.
    fn anchors(&self) -> &[Anchor] {
        if self.anchor_variants.is_empty() {
            std::slice::from_ref(&self.anchor)
        } else {
            &self.anchor_variants
        }
    }

    /// Upper bound (px) on how far a label reaches from its anchor, estimated
    /// before shaping. The widest an unshaped label can lay out is 2 em of
    /// advance per char — above any real glyph advance, ligatures and wide
    /// scripts included. Added on top: the collision `padding`, the halo (drawn
    /// beyond the collision box), and the widest shift an anchor candidate can
    /// apply (`radial-offset` or `offset-em`).
    ///
    /// `cap` bounds the advance by `max-extent-px` for point placement, where
    /// labels reaching past it are culled; line placement passes `false`, since
    /// a line label lays out along the path un-wrapped and legitimately extends
    /// half its length from each anchor, well past `max-extent-px`.
    fn label_reach(
        &self,
        sections: &[LabelSection],
        size: f32,
        padding: f32,
        halo: f32,
        cap: bool,
    ) -> f32 {
        let advance: f32 = sections
            .iter()
            .map(|s| s.text.chars().count() as f32 * 2.0 * size * s.scale)
            .sum();
        let advance = if cap {
            advance.min(self.max_extent_px)
        } else {
            advance
        };
        let offset = self
            .radial_offset_em
            .max(self.offset_em[0].abs().max(self.offset_em[1].abs()));
        advance + padding + halo + offset * size
    }

    /// Resolve a feature group's evaluated `text` into label sections against
    /// `base_font_id`, or `None` to skip (null/other types, empty label, or a
    /// constant with no text). Shared by the reach pre-scan and the main build.
    fn eval_sections(
        &self,
        ectx: &maplibre_expr::EvaluationContext,
        registry: &FontRegistry,
        base_font_id: u32,
    ) -> Option<Vec<LabelSection>> {
        match &self.text_expr {
            Some(e) => match maplibre_expr::evaluate(e, ectx) {
                Ok(v) => label_sections(&v, registry, base_font_id),
                _ => None,
            },
            None => match &self.text {
                Some(t) if !t.is_empty() => Some(vec![LabelSection {
                    text: t.clone(),
                    font_id: base_font_id,
                    scale: 1.0,
                    color: None,
                    valign: VerticalAlign::Baseline,
                }]),
                _ => None,
            },
        }
    }

    /// Evaluate a feature group once into a [`GroupPrep`] — its label sections
    /// and per-group paint/layout scalars, ready to shape — or `None` if it
    /// produces no label. `cap` matches [`Self::label_reach`]. Evaluating here
    /// (rather than in the shaping loop) lets the neighbour prefilter size its
    /// band from the widest label without a second evaluation pass.
    #[allow(clippy::too_many_arguments)]
    fn prep_group<'g>(
        &self,
        group: &'g FeatureGroup,
        feature: u32,
        dx: i64,
        dy: i64,
        registry: &FontRegistry,
        z: u8,
        const_size: f32,
        const_color: [f32; 4],
        const_halo_color: [f32; 4],
        const_halo_width: f32,
        const_opacity: f32,
        cap: bool,
    ) -> Option<GroupPrep<'g>> {
        let ectx = crate::render::group_expr_context(group, z);
        let base_font_id = registry.resolve(self.group_stack_key(&ectx).as_deref()).0;
        let font_fallback = self.font_expr.is_some() && base_font_id == 0;
        let size = eval_number(&self.size_expr, &ectx, const_size).max(0.0);
        // A group with no icon and nothing to say produces no symbol at all;
        // with an icon it still places, text or not (MapLibre's icon-only
        // symbol).
        let icon = self.icon.as_ref().and_then(|c| c.eval_group(&ectx));
        let mut sections = match size > 0.0 {
            true => self
                .eval_sections(&ectx, registry, base_font_id)
                .unwrap_or_default(),
            false => Vec::new(),
        };
        let mut text: String = sections.iter().map(|s| s.text.as_str()).collect();
        if text.is_empty() {
            sections.clear();
        }
        match (sections.is_empty(), &icon) {
            (true, None) => return None,
            (true, Some(i)) => text = i.name.clone(),
            _ => {}
        }
        let sort_key = match &self.sort_key_expr {
            Some(e) => match maplibre_expr::evaluate(e, &ectx) {
                Ok(maplibre_expr::Value::Number(n)) => n,
                _ => 0.0,
            },
            None => 0.0,
        };
        let opacity = eval_number(&self.opacity_expr, &ectx, const_opacity).clamp(0.0, 1.0);
        let padding = eval_number(&self.padding_expr, &ectx, self.padding_px).max(0.0);
        let mut color = eval_color(&self.color_expr, &ectx, const_color);
        let mut halo_color = eval_color(&self.halo_color_expr, &ectx, const_halo_color);
        color[3] *= opacity;
        halo_color[3] *= opacity;
        let halo_width = eval_number(&self.halo_width_expr, &ectx, const_halo_width).max(0.0);
        // The icon's contribution to the neighbour band is added by the
        // caller, which knows the sprite sheet's dimensions.
        let reach = self.label_reach(&sections, size, padding, halo_width, cap);
        Some(GroupPrep {
            group,
            dx,
            dy,
            feature,
            sections,
            text,
            icon,
            size,
            sort_key,
            opacity,
            padding,
            color,
            halo_color,
            halo_width,
            reach,
            font_fallback,
        })
    }

    /// Gather the 8 neighbour tiles' feature groups for cross-tile collision:
    /// each bound `<source>.<layer>@dx,dy` layer, filtered exactly like this
    /// tile's own features, paired with its `(dx, dy)` offset. Empty when
    /// collision is off, no upstream source is set, or nothing is bound;
    /// extent-mismatched layers (which would break the shared world frame) are
    /// skipped. Decoded once and reused by the reach pre-scan and the build.
    fn neighbor_groups(
        &self,
        ctx: &EvalCtx<'_>,
        z: u8,
        extent_i: i64,
    ) -> Vec<(Vec<FeatureGroup>, i64, i64)> {
        let mut out = Vec::new();
        if !self.collide {
            return out;
        }
        let Some(base) = &self.neighbor_base else {
            return out;
        };
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                if dx == 0 && dy == 0 {
                    continue;
                }
                let name = ezu_graph::neighbor_binding(base, dx, dy);
                let shared = match ctx.assets.load(&name) {
                    Ok(Asset::Features(opq)) => opq.downcast::<SharedLayer>().ok(),
                    _ => None,
                };
                let Some(shared) = shared else { continue };
                if shared.layer.extent.max(1) as i64 != extent_i {
                    continue;
                }
                let groups =
                    collect_groups(&shared, self.filter_expr.as_ref(), &self.min_zoom_field, z);
                out.push((groups, dx as i64, dy as i64));
            }
        }
        out
    }

    /// Layout params for line placement: a single un-wrapped line,
    /// centred so each glyph's `x` is measured from the label centre. The
    /// `offset-em` shift is applied during the walk (along + perpendicular
    /// to the line), not baked into the block.
    fn line_layout_params(&self) -> LayoutParams {
        LayoutParams {
            max_width_em: 0.0,
            line_height_em: self.line_height,
            letter_spacing_em: self.letter_spacing_em,
            anchor: Anchor::Center,
            justify: Justify::Center,
            offset_em: [0.0, 0.0],
            transform: self.transform,
        }
    }

    /// Load one stack of font asset keys into `StackEntry`s. Outline fonts and
    /// SDF glyph stacks mix; the draw path is picked per glyph by its backend.
    fn load_stack(&self, ctx: &EvalCtx<'_>, keys: &[String]) -> Result<Vec<StackEntry>, EvalError> {
        let mut fonts: Vec<StackEntry> = Vec::with_capacity(keys.len());
        for key in keys {
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
        Ok(fonts)
    }

    /// Load the default stack plus every registry stack, once per eval.
    /// Registries are small (a handful of stacks) and `ctx.assets.load` is a
    /// map lookup for pre-bound assets, so loading all up front is cheap and
    /// keeps neighbour-candidate resolution trivially symmetric.
    fn load_registry(&self, ctx: &EvalCtx<'_>) -> Result<FontRegistry, EvalError> {
        let default = self.load_stack(ctx, &self.font_keys)?;
        let mut stacks = Vec::with_capacity(self.font_stacks.len());
        let mut by_key = HashMap::with_capacity(self.font_stacks.len());
        for (i, (key, keys)) in self.font_stacks.iter().enumerate() {
            stacks.push(self.load_stack(ctx, keys)?);
            by_key.insert(key.clone(), i);
        }
        Ok(FontRegistry {
            default,
            stacks,
            by_key,
        })
    }

    /// (A) Evaluate `font-expr` for a feature group into a canonical stack key.
    /// `None` when there is no expression, it doesn't evaluate to an array of
    /// strings, or the array is empty — the caller then uses the default stack.
    fn group_stack_key(&self, ectx: &maplibre_expr::EvaluationContext) -> Option<String> {
        let expr = self.font_expr.as_ref()?;
        let items = match maplibre_expr::evaluate(expr, ectx) {
            Ok(maplibre_expr::Value::Array(items)) => items,
            _ => return None,
        };
        let mut names = Vec::with_capacity(items.len());
        for v in &items {
            match v {
                maplibre_expr::Value::String(s) => names.push(s.as_str()),
                _ => return None,
            }
        }
        if names.is_empty() {
            return None;
        }
        Some(stack_key(names.into_iter()))
    }

    /// Line / line-center placement: shape each label once, then walk
    /// every candidate polyline generating tangent-rotated glyph runs, and
    /// collect them as placement candidates (per-glyph boxes, so the label
    /// is all-or-nothing). Determinism mirrors the point path: candidate
    /// lines come from this tile plus its neighbours, and every quantity
    /// feeding placement is derived from the shared world-pixel frame.
    fn line_labels(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
        feats: &crate::nodes::common::FilteredFeatures,
        registry: &FontRegistry,
        faces: &FaceCache<'_>,
    ) -> Result<LabelSet, EvalError> {
        let mode = self.placement.line().expect("called on line placement");
        let const_size = (self.size.get(ctx, inputs)? as f32).max(0.0);
        let const_color = self.color.get(ctx, inputs)?;
        let const_halo_color = self.halo_color.get(ctx, inputs)?;
        let const_halo_width = (self.halo_width.get(ctx, inputs)? as f32).max(0.0);
        let const_opacity = (self.opacity.get(ctx, inputs)? as f32).clamp(0.0, 1.0);

        let tile_w = ctx.canvas.tile_w as f32;
        let tile_h = ctx.canvas.tile_h as f32;
        let extent_i = feats.extent.max(1) as i64;
        let sx = tile_w / extent_i as f32;
        let sy = tile_h / extent_i as f32;
        let z = ctx.tile.z;
        let params = self.line_layout_params();
        let (tx, ty) = (ctx.tile.x as i64, ctx.tile.y as i64);

        // Keyed by the content hash (sections' text/font/scale) + size, so a
        // data-driven `font-expr` or per-section `format` fonts lay the same
        // text out against different stacks without cache collisions.
        let mut blocks: HashMap<(u64, u32), Arc<TextBlock>> = HashMap::new();
        let mut dropped_chars = 0usize;
        let mut missing_range_chars = 0usize;
        let mut font_fallbacks = 0usize;
        // Process-wide layout-cache activity, this eval (outline stacks only).
        let mut layout_hits = 0usize;
        let mut layout_misses = 0usize;

        let mut cands: Vec<LabelCandidate> = Vec::new();
        let mut draws: Vec<LabelDraw> = Vec::new();

        // Gather the neighbour feature groups once (collision only), decoded
        // here and reused by the single evaluation pass below.
        let nbr_groups = self.neighbor_groups(ctx, z, extent_i);

        // Evaluate every own and neighbour group once, up front — label sections
        // and per-group paint scalars, never the shaping — tracking the widest
        // label reach for the neighbour band and the per-own-label warnings.
        let mut preps: Vec<GroupPrep> = Vec::new();
        let mut reach_max = 0.0f32;
        for (fi, group) in feats.groups.iter().enumerate() {
            if group.lines.is_empty() {
                continue;
            }
            if let Some(prep) = self.prep_group(
                group,
                fi as u32,
                0,
                0,
                registry,
                z,
                const_size,
                const_color,
                const_halo_color,
                const_halo_width,
                const_opacity,
                false,
            ) {
                reach_max = reach_max.max(prep.reach);
                if prep.font_fallback {
                    font_fallbacks += 1;
                }
                preps.push(prep);
            }
        }
        for (groups, dx, dy) in &nbr_groups {
            for (fi, group) in groups.iter().enumerate() {
                if group.lines.is_empty() {
                    continue;
                }
                if let Some(prep) = self.prep_group(
                    group,
                    fi as u32,
                    *dx,
                    *dy,
                    registry,
                    z,
                    const_size,
                    const_color,
                    const_halo_color,
                    const_halo_width,
                    const_opacity,
                    false,
                ) {
                    reach_max = reach_max.max(prep.reach);
                    preps.push(prep);
                }
            }
        }

        // Neighbour prefilter band: this many multiples of the widest label
        // reach — a collision chain advances one reach per hop, and the repeat
        // filter reaches half a spacing per hop. A neighbour line whose
        // geometry stays outside this band of the tile cannot change what it
        // draws, so shaping it is wasted; skip it.
        // MapLibre's repeat distance, half the requested spacing: it keeps the
        // same street name off every branch of its own road. Line-center
        // places one label per line, so it doesn't apply there.
        let repeat_px = match mode {
            LinePlacement::Line => 0.5 * self.spacing_px,
            LinePlacement::LineCenter => 0.0,
        };
        let band = NEIGHBOR_BAND_HOPS * reach_max.max(repeat_px);

        // Shape the surviving groups into placement candidates. Every collision
        // input is in the local world-pixel frame (current tile origin
        // subtracted), so a neighbour tile agrees on each straddling label.
        for prep in &preps {
            let dx = prep.dx;
            let dy = prep.dy;
            let group = prep.group;
            let sections = &prep.sections;
            let text = &prep.text;
            let size = prep.size;
            let sort_key = prep.sort_key;
            let opacity = prep.opacity;
            let padding = prep.padding;
            let color = prep.color;
            let halo_color = prep.halo_color;
            let halo_width = prep.halo_width;
            // Neighbour prefilter: skip a neighbour line whose geometry lies
            // outside the collision band — its anchors slide along the path, so
            // the whole polyline bbox is the reach reference.
            if dx != 0 || dy != 0 {
                let (mut min_x, mut min_y, mut max_x, mut max_y) =
                    (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for line in &group.lines {
                    for &(x, y) in line {
                        let lpx = (dx * extent_i + x as i64) as f32 * sx;
                        let lpy = (dy * extent_i + y as i64) as f32 * sy;
                        min_x = min_x.min(lpx);
                        max_x = max_x.max(lpx);
                        min_y = min_y.min(lpy);
                        max_y = max_y.max(lpy);
                    }
                }
                if !bbox_within_band(min_x, min_y, max_x, max_y, tile_w, tile_h, band) {
                    continue;
                }
            }

            let (flat_fonts, ranges, hash) = assemble_stacks(sections, registry);
            let key = (hash, size.to_bits());
            let block = match blocks.get(&key) {
                Some(b) => b.clone(),
                None => {
                    // Geometric prefilter (no shaping): line placement can only
                    // anchor a label on a polyline at least as long as the label
                    // (`generate_anchors` yields nothing once the label is longer
                    // than the line). If even the longest polyline in the group
                    // is shorter than a lower bound on the label's shaped width,
                    // no anchor can ever be generated here — skip shaping.
                    let view = faces.view(&flat_fonts);
                    let lb_width_px = line_lower_bound_width_em(
                        sections,
                        &ranges,
                        &view,
                        self.transform,
                        self.letter_spacing_em,
                    ) * size;
                    let mut l_max = 0.0f32;
                    for line in &group.lines {
                        if line.len() < 2 {
                            continue;
                        }
                        let (x0, y0) = line[0];
                        let mut prev = (
                            (dx * extent_i + x0 as i64) as f32 * sx,
                            (dy * extent_i + y0 as i64) as f32 * sy,
                        );
                        let mut acc = 0.0f32;
                        for &(x, y) in &line[1..] {
                            let cur = (
                                (dx * extent_i + x as i64) as f32 * sx,
                                (dy * extent_i + y as i64) as f32 * sy,
                            );
                            let (ddx, ddy) = (cur.0 - prev.0, cur.1 - prev.1);
                            acc += (ddx * ddx + ddy * ddy).sqrt();
                            prev = cur;
                        }
                        if acc > l_max {
                            l_max = acc;
                        }
                    }
                    if lb_width_px > l_max {
                        continue;
                    }
                    let build = || {
                        let specs: Vec<SectionSpec<'_>> = sections
                            .iter()
                            .zip(&ranges)
                            .map(|(s, r)| SectionSpec {
                                text: &s.text,
                                fonts: r.clone(),
                                scale: s.scale,
                                valign: s.valign,
                            })
                            .collect();
                        layout_sections(&specs, &view, &params)
                    };
                    // Reuse an identically-laid-out block across tiles via the
                    // process-wide cache (outline stacks only); otherwise build.
                    let b = match layout_cache_key(&flat_fonts, sections, &ranges, size, &params) {
                        Some(gk) => {
                            let (b, hit) = get_or_build_layout(gk, build);
                            if hit {
                                layout_hits += 1;
                            } else {
                                layout_misses += 1;
                            }
                            b
                        }
                        None => Arc::new(build()),
                    };
                    blocks.insert(key, b.clone());
                    b
                }
            };
            if dx == 0 && dy == 0 {
                dropped_chars += block.dropped_chars;
                missing_range_chars += block.missing_range_chars;
            }
            if block.is_empty() {
                continue;
            }
            let fonts = Arc::new(flat_fonts);
            let paints = Arc::new(section_paints(sections, opacity));

            // Each glyph's horizontal-centre offset (px) from the label
            // centre, plus the along-line (`offset-em[0]`) shift; the
            // perpendicular (`offset-em[1]`) shift is handled at draw.
            let along = self.offset_em[0] * size;
            let perp = self.offset_em[1] * size;
            let centre_offsets: Vec<f32> = block
                .glyphs
                .iter()
                .map(|g| (g.x + 0.5 * g.advance) * size + along)
                .collect();
            let total_len = block.bbox.width() * size;
            if total_len <= 0.0 {
                continue;
            }
            // A glyph's collision half-height spans the line box plus the
            // perpendicular offset.
            let half_h = 0.5 * block.bbox.height() * size + perp.abs();
            let paint = TextPaint {
                size_px: size,
                color,
                halo_color,
                halo_width_px: halo_width,
                halo_blur_px: 0.0,
            };
            let anchor_params = AnchorParams {
                placement: mode,
                label_len: total_len,
                spacing: self.spacing_px,
                max_angle_deg: self.max_angle_deg,
                // MapLibre measures the bend over 3/5 of the font size, so a
                // long gentle curve is fine and only a kink is rejected.
                angle_window: ANGLE_WINDOW_EM * size,
                glyph_size: size,
                // Set per clipped run below.
                continued: false,
            };

            // MapLibre's within-feature symbol order: the anchors of each of
            // the feature's lines, in the order they were generated.
            let mut symbol = 0u32;
            for line in &group.lines {
                if line.len() < 2 {
                    continue;
                }
                let full: Vec<(f32, f32)> = line
                    .iter()
                    .map(|&(x, y)| {
                        let wx = dx * extent_i + x as i64;
                        let wy = dy * extent_i + y as i64;
                        (wx as f32 * sx, wy as f32 * sy)
                    })
                    .collect();
                // MapLibre anchors on the geometry clipped to the tile the
                // feature came from, so the copy of a road in this tile and
                // the copy in its neighbour's buffer each label only their
                // own stretch.
                let (tile_x0, tile_y0) = (dx as f32 * tile_w, dy as f32 * tile_h);
                let runs = clip_line(&full, tile_x0, tile_y0, tile_x0 + tile_w, tile_y0 + tile_h);
                for poly in &runs {
                    let head = poly[0];
                    let mut params = anchor_params;
                    params.continued = head.0 <= tile_x0
                        || head.0 >= tile_x0 + tile_w
                        || head.1 <= tile_y0
                        || head.1 >= tile_y0 + tile_h;
                    for mut anchor in generate_anchors(poly, &params) {
                        if !self.keep_upright {
                            anchor.reversed = false;
                        }
                        let Some(placed) = place_glyphs(poly, &anchor, &centre_offsets) else {
                            continue;
                        };
                        // Per-glyph rotated-AABB collision boxes (local px).
                        let mut boxes = Vec::with_capacity(placed.len());
                        for (g, gp) in block.glyphs.iter().zip(&placed) {
                            let hw = 0.5 * g.advance * size;
                            let (c, s) = (gp.angle.cos().abs(), gp.angle.sin().abs());
                            let ex = hw * c + half_h * s;
                            let ey = hw * s + half_h * c;
                            boxes.push(
                                Aabb {
                                    min_x: gp.x - ex,
                                    min_y: gp.y - ey,
                                    max_x: gp.x + ex,
                                    max_y: gp.y + ey,
                                }
                                .inflate(padding),
                            );
                        }
                        // World anchor (extent units) of the label-centre sample.
                        let world_ax = tx * extent_i + (anchor.x / sx).round() as i64;
                        let world_ay = ty * extent_i + (anchor.y / sy).round() as i64;
                        cands.push(LabelCandidate {
                            sort_key,
                            rank: PlaceRank {
                                tile: (tx + dx, ty + dy),
                                feature: prep.feature,
                                symbol,
                            },
                            world_ax,
                            world_ay,
                            text: text.clone(),
                            style_id: hash,
                            // One variant holding every glyph box: the label shows
                            // only where the whole run is free.
                            variants: vec![boxes],
                            anchor_x: anchor.x,
                            anchor_y: anchor.y,
                            repeat_px,
                            allow_overlap: self.allow_overlap,
                            ignore_placement: self.ignore_placement,
                        });
                        let placements: Vec<GlyphPlacement> = placed
                            .iter()
                            .map(|g| GlyphPlacement {
                                x: g.x,
                                y: g.y,
                                angle: g.angle,
                            })
                            .collect();
                        symbol += 1;
                        draws.push(LabelDraw::Line {
                            block: block.clone(),
                            placements,
                            perp_px: perp,
                            paint,
                            fonts: fonts.clone(),
                            paints: paints.clone(),
                        });
                    }
                }
            }
        }

        log_layout_cache_stats(layout_hits, layout_misses);

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
        if font_fallbacks > 0 {
            tracing::warn!(
                "text: {font_fallbacks} label(s) named a font stack not in `font-stacks` — \
                 the default stack was used"
            );
        }

        Ok(self.label_set(cands, draws))
    }

    /// Wrap this node's candidates and draw payloads into a [`LabelSet`],
    /// stamping the content identity a shared placement matches on.
    /// Point placement: shape every own and neighbour label once and collect
    /// them as placement candidates. Everything that feeds the collision
    /// decision is derived from the world anchor (exact integer tile-frame
    /// coordinate) and the em box × size — never from tile-local floats — so
    /// adjacent tiles agree.
    fn point_labels(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
        feats: &crate::nodes::common::FilteredFeatures,
        registry: &FontRegistry,
        faces: &FaceCache<'_>,
    ) -> Result<LabelSet, EvalError> {
        // Constants, resolved once. Data-driven exprs (if present)
        // override these per feature group.
        let const_size = (self.size.get(ctx, inputs)? as f32).max(0.0);
        let const_color = self.color.get(ctx, inputs)?;
        let const_halo_color = self.halo_color.get(ctx, inputs)?;
        let const_halo_width = (self.halo_width.get(ctx, inputs)? as f32).max(0.0);
        let const_opacity = (self.opacity.get(ctx, inputs)? as f32).clamp(0.0, 1.0);

        let tile_w = ctx.canvas.tile_w as f32;
        let tile_h = ctx.canvas.tile_h as f32;
        let extent_i = feats.extent.max(1) as i64;
        let sx = tile_w / extent_i as f32;
        let sy = tile_h / extent_i as f32;
        let z = ctx.tile.z;
        let (tx, ty) = (ctx.tile.x as i64, ctx.tile.y as i64);

        // Shaping is the expensive step; a label is laid out once per eval no
        // matter how many groups/points repeat it. The key is the content hash
        // (folding each section's text, font, and scale) plus the size, so
        // neighbour candidates share the cache (identical exprs → identical
        // layout across tiles) while differently-styled same-text labels stay
        // distinct.
        // A label is laid out once per (content, size, anchor); a
        // `text-variable-anchor` label shares every non-anchor input across its
        // anchor candidates, so only the anchor varies the layout.
        let mut blocks: HashMap<(u64, u32, u8), Arc<TextBlock>> = HashMap::new();
        let mut culled = 0usize;
        let mut dropped_chars = 0usize;
        let mut missing_range_chars = 0usize;
        let mut font_fallbacks = 0usize;
        // Process-wide layout-cache activity, this eval (outline stacks only).
        let mut layout_hits = 0usize;
        let mut layout_misses = 0usize;

        let mut cands: Vec<LabelCandidate> = Vec::new();
        let mut draws: Vec<LabelDraw> = Vec::new();

        // Gather the neighbour feature groups once (collision only): each bound
        // neighbour layer, filtered exactly like this tile's own features.
        let nbr_groups = self.neighbor_groups(ctx, z, extent_i);

        // Evaluate every own and neighbour group once, up front — label sections
        // and per-group paint scalars, never the shaping — tracking the widest
        // label reach for the neighbour band and the per-own-label warnings.
        let mut preps: Vec<GroupPrep> = Vec::new();
        let mut reach_max = 0.0f32;
        for (fi, group) in feats.groups.iter().enumerate() {
            if group.points.is_empty() {
                continue;
            }
            if let Some(prep) = self.prep_group(
                group,
                fi as u32,
                0,
                0,
                registry,
                z,
                const_size,
                const_color,
                const_halo_color,
                const_halo_width,
                const_opacity,
                true,
            ) {
                reach_max = reach_max.max(prep.reach);
                if prep.font_fallback {
                    font_fallbacks += 1;
                }
                preps.push(prep);
            }
        }
        for (groups, dx, dy) in &nbr_groups {
            for (fi, group) in groups.iter().enumerate() {
                if group.points.is_empty() {
                    continue;
                }
                if let Some(prep) = self.prep_group(
                    group,
                    fi as u32,
                    *dx,
                    *dy,
                    registry,
                    z,
                    const_size,
                    const_color,
                    const_halo_color,
                    const_halo_width,
                    const_opacity,
                    true,
                ) {
                    reach_max = reach_max.max(prep.reach);
                    preps.push(prep);
                }
            }
        }

        // The sprite sheet, loaded once per eval and only when some group
        // actually names an icon (a layer whose `icon-image` resolves empty
        // everywhere costs nothing).
        let sheet = match &self.icon {
            Some(cfg) if preps.iter().any(|p| p.icon.is_some()) => {
                let Asset::Sprite(sheet) = ctx.assets.load(&cfg.sprite)? else {
                    return Err(EvalError::Other(format!(
                        "asset `{}` is not a sprite sheet",
                        cfg.sprite
                    )));
                };
                Some(sheet)
            }
            _ => None,
        };
        // An icon reaches at most half the sheet's widest icon from its
        // anchor, plus its offset and padding — the icon half of the
        // neighbour band.
        if let (Some(cfg), Some(sheet)) = (&self.icon, &sheet) {
            let widest = sheet
                .icons
                .values()
                .map(|r| r.width.max(r.height) as f32 / r.pixel_ratio.max(f32::EPSILON))
                .fold(0.0f32, f32::max);
            let off = cfg.offset[0].abs().max(cfg.offset[1].abs());
            for prep in &preps {
                if let Some(icon) = &prep.icon {
                    reach_max = reach_max.max((0.5 * widest + off) * icon.size + icon.padding);
                }
            }
        }

        // Neighbour prefilter band: this many multiples of the widest label
        // reach — a collision chain advances one reach per hop. A neighbour
        // feature whose geometry stays outside this band of the tile cannot
        // change what it draws, so shaping it is wasted; skip it.
        let band = NEIGHBOR_BAND_HOPS * reach_max;
        // One cropped image per icon name, shared by every symbol using it.
        let mut crops: HashMap<String, Option<(Arc<ezu_graph::RasterBuf>, f32)>> = HashMap::new();

        // Shape the surviving groups into placement candidates. Everything that
        // feeds the collision decision is derived from the world anchor (exact
        // integer tile-frame coordinate) and the em box × size — never from
        // tile-local floats — so adjacent tiles agree.
        for prep in &preps {
            let dx = prep.dx;
            let dy = prep.dy;
            let group = prep.group;
            let sections = &prep.sections;
            let text = &prep.text;
            let size = prep.size;
            let sort_key = prep.sort_key;
            let opacity = prep.opacity;
            let padding = prep.padding;
            let color = prep.color;
            let halo_color = prep.halo_color;
            let halo_width = prep.halo_width;
            // Neighbour prefilter: skip a neighbour feature whose geometry lies
            // outside the collision band — it cannot change what this tile draws.
            if dx != 0 || dy != 0 {
                let (mut min_x, mut min_y, mut max_x, mut max_y) =
                    (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for &(x, y) in &group.points {
                    let lpx = (dx * extent_i + x as i64) as f32 * sx;
                    let lpy = (dy * extent_i + y as i64) as f32 * sy;
                    min_x = min_x.min(lpx);
                    max_x = max_x.max(lpx);
                    min_y = min_y.min(lpy);
                    max_y = max_y.max(lpy);
                }
                if !bbox_within_band(min_x, min_y, max_x, max_y, tile_w, tile_h, band) {
                    continue;
                }
            }

            let (flat_fonts, ranges, hash) = assemble_stacks(sections, registry);
            // Lay out one block per anchor candidate (a single fixed anchor,
            // or the `text-variable-anchor` list). Each is cached by anchor so
            // repeated points/neighbours reuse it. An icon-only symbol has no
            // sections and lays nothing out.
            let mut text_blocks: Vec<Arc<TextBlock>> = if sections.is_empty() {
                Vec::new()
            } else {
                self.anchors()
                    .iter()
                    .map(|&anchor| {
                        let pkey = (hash, size.to_bits(), anchor as u8);
                        if let Some(b) = blocks.get(&pkey) {
                            return b.clone();
                        }
                        let params = self.variant_layout_params(anchor);
                        let build = || {
                            let specs: Vec<SectionSpec<'_>> = sections
                                .iter()
                                .zip(&ranges)
                                .map(|(s, r)| SectionSpec {
                                    text: &s.text,
                                    fonts: r.clone(),
                                    scale: s.scale,
                                    valign: s.valign,
                                })
                                .collect();
                            let view = faces.view(&flat_fonts);
                            layout_sections(&specs, &view, &params)
                        };
                        // Reuse an identically-laid-out block across tiles via the
                        // process-wide cache (outline stacks only); otherwise build.
                        let block =
                            match layout_cache_key(&flat_fonts, sections, &ranges, size, &params) {
                                Some(gk) => {
                                    let (b, hit) = get_or_build_layout(gk, build);
                                    if hit {
                                        layout_hits += 1;
                                    } else {
                                        layout_misses += 1;
                                    }
                                    b
                                }
                                None => Arc::new(build()),
                            };
                        blocks.insert(pkey, block.clone());
                        block
                    })
                    .collect()
            };
            if let Some(primary) = text_blocks.first() {
                // The primary anchor drives the warning counts and the cull
                // test; every variant shares the same glyphs, so it is
                // representative.
                // Count layout warnings once per distinct label (own groups
                // only; neighbours repeat the same strings).
                if dx == 0 && dy == 0 {
                    dropped_chars += primary.dropped_chars;
                    missing_range_chars += primary.missing_range_chars;
                }
                // A label reaching past the pad this node requested would clip
                // at tile borders — drop the text instead. The symbol's icon,
                // if any, still places.
                let b = primary.bbox;
                let half_extent = [b.min_x, b.max_x, b.min_y, b.max_y]
                    .iter()
                    .fold(0.0f32, |m, v| m.max(v.abs()))
                    * size;
                if half_extent > self.max_extent_px {
                    if dx == 0 && dy == 0 {
                        culled += group.points.len();
                    }
                    text_blocks.clear();
                } else if primary.is_empty() {
                    text_blocks.clear();
                }
            }
            // The group's icon, cropped once and shared by its points, with
            // the collision box it occupies relative to the symbol anchor.
            // With `icon-text-fit` the sprite is stretched to the label box
            // instead, once per anchor candidate — each lays the text out in
            // a different place, so each needs its own fitted icon.
            let cropped = match (&self.icon, &prep.icon, &sheet) {
                (Some(_), Some(gi), Some(sheet)) => crops
                    .entry(gi.name.clone())
                    .or_insert_with(|| {
                        let ratio = sheet
                            .icons
                            .get(&gi.name)
                            .map_or(1.0, |r| r.pixel_ratio.max(f32::EPSILON));
                        sheet.crop(&gi.name).map(|img| (Arc::new(img), ratio))
                    })
                    .clone(),
                _ => None,
            };
            let mut icons: Vec<(Arc<IconDraw>, Aabb)> = Vec::new();
            if let (Some(cfg), Some(gi), Some((image, ratio)), Some(sheet)) =
                (&self.icon, &prep.icon, &cropped, &sheet)
            {
                // MapLibre displays a sprite at its authored size — atlas
                // pixels over the sheet's pixel ratio — times `icon-size`.
                let scale = gi.size / ratio;
                let fit_blocks: &[Arc<TextBlock>] = match cfg.text_fit {
                    IconTextFit::None => &[],
                    _ => &text_blocks,
                };
                if fit_blocks.is_empty() {
                    icons.push(plain_icon(cfg, gi, image.clone(), scale));
                } else {
                    let rect = sheet.icons.get(&gi.name);
                    for block in fit_blocks {
                        icons.push(fitted_icon(cfg, gi, image, rect, scale, block, size));
                    }
                }
            }
            if text_blocks.is_empty() && icons.is_empty() {
                continue;
            }
            // Two symbols differing only in their icon are distinct labels, so
            // the icon joins the style identity that dedup and ordering key on.
            let style_id = match &prep.icon {
                Some(gi) => {
                    let mut h = Xxh3::new();
                    h.update(&hash.to_le_bytes());
                    h.update(gi.name.as_bytes());
                    h.update(&gi.size.to_bits().to_le_bytes());
                    h.digest()
                }
                None => hash,
            };
            // A symbol carries one overlap decision. With both halves present
            // the stricter of the two wins — MapLibre's per-box flags have no
            // single-candidate equivalent.
            let (allow_overlap, ignore_placement) = match (&self.icon, !icons.is_empty()) {
                (Some(cfg), true) if text_blocks.is_empty() => {
                    (cfg.allow_overlap, cfg.ignore_placement)
                }
                (Some(cfg), true) => (
                    self.allow_overlap && cfg.allow_overlap,
                    self.ignore_placement && cfg.ignore_placement,
                ),
                _ => (self.allow_overlap, self.ignore_placement),
            };
            let paint = TextPaint {
                size_px: size,
                color,
                halo_color,
                halo_width_px: halo_width,
                halo_blur_px: 0.0,
            };
            let fonts = Arc::new(flat_fonts);
            let paints = Arc::new(section_paints(sections, opacity));
            // A variant's collision box at a point: its em bbox scaled to px,
            // offset to the point, inflated by `padding-px`.
            let box_at = |block: &TextBlock, lpx: f32, lpy: f32| -> Aabb {
                let bb = block.bbox;
                Aabb {
                    min_x: lpx + bb.min_x * size,
                    min_y: lpy + bb.min_y * size,
                    max_x: lpx + bb.max_x * size,
                    max_y: lpy + bb.max_y * size,
                }
                .inflate(padding)
            };
            for (pi, &(x, y)) in group.points.iter().enumerate() {
                let world_ax = (tx + dx) * extent_i + x as i64;
                let world_ay = (ty + dy) * extent_i + y as i64;
                // Local world-pixel frame (current tile origin subtracted):
                // small magnitudes, and translation-invariant so a
                // neighbour tile — which subtracts its own origin — reaches
                // identical collision decisions.
                let lpx = (world_ax - tx * extent_i) as f32 * sx;
                let lpy = (world_ay - ty * extent_i) as f32 * sy;
                // One box per anchor candidate, in declaration order, joined
                // with the icon's into the symbol's variants: the symbol takes
                // the first variant whose every box is free.
                let text_boxes: Vec<Aabb> =
                    text_blocks.iter().map(|v| box_at(v, lpx, lpy)).collect();
                let icon_boxes: Vec<Aabb> = icons
                    .iter()
                    .map(|(_, rel)| Aabb {
                        min_x: rel.min_x + lpx,
                        min_y: rel.min_y + lpy,
                        max_x: rel.max_x + lpx,
                        max_y: rel.max_y + lpy,
                    })
                    .collect();
                let (variant_boxes, point_variants) = symbol_variants(
                    &text_boxes,
                    &icon_boxes,
                    self.text_optional,
                    self.icon.as_ref().is_some_and(|c| c.optional),
                );
                cands.push(LabelCandidate {
                    sort_key,
                    rank: PlaceRank {
                        tile: (tx + dx, ty + dy),
                        feature: prep.feature,
                        symbol: pi as u32,
                    },
                    world_ax,
                    world_ay,
                    text: text.clone(),
                    style_id,
                    variants: variant_boxes,
                    anchor_x: lpx,
                    anchor_y: lpy,
                    // The repeat filter is a line-label rule (MapLibre records
                    // anchors along a path); a point label has one anchor.
                    repeat_px: 0.0,
                    allow_overlap,
                    ignore_placement,
                });
                draws.push(LabelDraw::Point {
                    blocks: text_blocks.clone(),
                    variants: point_variants,
                    anchor: (lpx, lpy),
                    paint,
                    fonts: fonts.clone(),
                    paints: paints.clone(),
                    icons: icons.iter().map(|(d, _)| d.clone()).collect(),
                });
            }
        }

        log_layout_cache_stats(layout_hits, layout_misses);
        // One summary line per eval, not one per label.
        if culled > 0 {
            tracing::warn!(
                "text: culled {culled} label placement(s) whose bbox exceeds max-extent-px ({}px)",
                self.max_extent_px
            );
        }
        if font_fallbacks > 0 {
            tracing::warn!(
                "text: {font_fallbacks} label(s) named a font stack not in `font-stacks` — \
                 the default stack was used"
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

        Ok(self.label_set(cands, draws))
    }

    /// The label set for one eval: the zoom gate and the "nothing to place"
    /// shortcuts, then the point or line candidate build.
    fn eval_labels(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<LabelSet, EvalError> {
        // Style-level zoom gate: outside the band the layer draws nothing,
        // neighbour candidates included.
        let tile_z = ctx.tile.z;
        if self.min_zoom.is_some_and(|mn| tile_z < mn)
            || self.max_zoom.is_some_and(|mx| tile_z > mx)
        {
            return Ok(self.label_set(Vec::new(), Vec::new()));
        }
        let feats = downcast_features(
            inputs[0]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("features".into()))?,
        )?;
        let line = self.placement.line().is_some();
        // With collision off and none of this layer's own geometry there is
        // nothing to place. With collision on, neighbour tiles may still spill
        // labels into this tile, so we proceed and decide from the gathered
        // candidates.
        let has_own = if line {
            feats.has_lines()
        } else {
            feats.has_points()
        };
        if !self.collide && !has_own {
            return Ok(self.label_set(Vec::new(), Vec::new()));
        }

        // Resolve the default stack plus every registry stack once per eval.
        // A per-feature `font-expr` picks among them; the draw path is picked
        // per glyph by each entry's backend.
        let registry = self.load_registry(ctx)?;
        // Build each outline font's face once; every label's flat stack shares
        // these fonts, so shaping/coverage/outline reuse them instead of
        // reparsing the font file per call.
        let faces = FaceCache::from_stacks(registry.all_stacks());
        if line {
            self.line_labels(ctx, inputs, &feats, &registry, &faces)
        } else {
            self.point_labels(ctx, inputs, &feats, &registry, &faces)
        }
    }

    /// Wrap this node's candidates and draw payloads into a [`LabelSet`],
    /// stamping the content identity a shared placement matches on.
    fn label_set(&self, candidates: Vec<LabelCandidate>, draws: Vec<LabelDraw>) -> LabelSet {
        let mut h = Xxh3::new();
        self.param_hash(&mut h);
        LabelSet {
            id: set_id(h.digest(), &candidates),
            candidates,
            draws,
            padding_px: self.padding_px,
            collide: self.collide,
            outline_sdf: self.outline_sdf,
        }
    }
}

impl Node for TextNode {
    fn op_name(&self) -> &'static str {
        match self.stage {
            Stage::Whole => "text",
            Stage::Labels => "text-labels",
        }
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        match self.stage {
            Stage::Whole => PortKind::Raster,
            Stage::Labels => PortKind::Labels,
        }
    }
    fn coord_space(&self) -> CoordSpace {
        CoordSpace::World
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        match self.stage {
            // The paired `text-draw` node requests the label pad; asking for it
            // again here would double-count it upstream.
            Stage::Labels => downstream,
            Stage::Whole => downstream + self.max_extent_px.max(0.0).ceil() as u32,
        }
    }
    fn asset_inputs(&self) -> Vec<String> {
        let mut keys = self.font_keys.clone();
        // Every stack a `font-expr` (or a `format` section) could pick must be
        // enumerable up front — wasm hosts pre-bind assets from this list.
        for (_, asset_keys) in &self.font_stacks {
            keys.extend(asset_keys.iter().cloned());
        }
        // The sprite sheet the icons are cropped from (wasm hosts pre-bind).
        if let Some(cfg) = &self.icon {
            keys.push(cfg.sprite.clone());
        }
        keys.sort();
        keys.dedup();
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
        let set = self.eval_labels(ctx, inputs)?;
        match self.stage {
            // Hand the candidates to the shared `label-placement` node.
            Stage::Labels => Ok(PortValue::Labels(Arc::new(set))),
            // Self-contained: place this layer's own candidates and draw.
            Stage::Whole => {
                let placed = if self.collide {
                    collide::place(&set.candidates, collide::COLLISION_CELL_PX)
                } else {
                    (0..set.candidates.len())
                        .map(|cand| collide::Placement { cand, variant: 0 })
                        .collect()
                };
                let faces = FaceCache::from_stacks(set.stacks());
                draw_labels(ctx, &set, &placed, &faces)
            }
        }
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"text");
        // The stage decides the output *kind*, so it has to key the cache:
        // otherwise a `text` and a `text-labels` node with identical fields
        // would share an entry and hand back the wrong value.
        h.update(&[self.stage as u8]);
        for key in &self.font_keys {
            h.update(key.as_bytes());
            h.update(&[0]);
        }
        // Dynamic font config — absent fields fold nothing, so existing
        // recipes keep their hashes (and caches).
        if let Some(s) = &self.font_expr_src {
            h.update(b"fontexpr");
            h.update(s.as_bytes());
        }
        for (key, asset_keys) in &self.font_stacks {
            h.update(b"fontstack");
            h.update(key.as_bytes());
            h.update(&[0]);
            for k in asset_keys {
                h.update(k.as_bytes());
                h.update(&[0]);
            }
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
            (b"paddingexpr".as_slice(), &self.padding_expr_src),
            (b"sortkeyexpr".as_slice(), &self.sort_key_expr_src),
            (b"filterexpr".as_slice(), &self.filter_expr_src),
        ] {
            if let Some(s) = src {
                h.update(tag);
                h.update(s.as_bytes());
            }
        }
        h.update(&[
            self.anchor as u8,
            self.justify as u8,
            self.transform as u8,
            self.placement as u8,
            self.keep_upright as u8,
        ]);
        h.update(b"anchorvariants");
        for a in &self.anchor_variants {
            h.update(&[*a as u8]);
        }
        h.update(&self.radial_offset_em.to_le_bytes());
        for v in [
            self.offset_em[0],
            self.offset_em[1],
            self.max_width_em,
            self.line_height,
            self.letter_spacing_em,
            self.max_extent_px,
            self.padding_px,
            self.spacing_px,
            self.max_angle_deg,
        ] {
            h.update(&v.to_le_bytes());
        }
        h.update(&[
            self.collide as u8,
            self.allow_overlap as u8,
            self.ignore_placement as u8,
            self.text_optional as u8,
        ]);
        // Icon config — absent fields fold nothing, so an icon-less node keeps
        // its hash (and its cache entries).
        if let Some(cfg) = &self.icon {
            h.update(b"icon");
            h.update(cfg.sprite.as_bytes());
            h.update(&[0]);
            if let Some(name) = &cfg.name {
                h.update(name.as_bytes());
                h.update(&[0]);
            }
            for (tag, src) in [
                (b"iconnameexpr".as_slice(), &cfg.name_expr_src),
                (b"iconsizeexpr".as_slice(), &cfg.size_expr_src),
                (b"iconrotateexpr".as_slice(), &cfg.rotate_expr_src),
                (b"iconopacityexpr".as_slice(), &cfg.opacity_expr_src),
                (b"iconpaddingexpr".as_slice(), &cfg.padding_expr_src),
            ] {
                if let Some(s) = src {
                    h.update(tag);
                    h.update(s.as_bytes());
                }
            }
            for v in [
                cfg.size,
                cfg.rotate_deg,
                cfg.opacity,
                cfg.offset[0],
                cfg.offset[1],
                cfg.padding_px,
                cfg.text_fit_padding[0],
                cfg.text_fit_padding[1],
                cfg.text_fit_padding[2],
                cfg.text_fit_padding[3],
            ] {
                h.update(&v.to_le_bytes());
            }
            h.update(&[
                cfg.anchor as u8,
                cfg.allow_overlap as u8,
                cfg.ignore_placement as u8,
                cfg.optional as u8,
                cfg.text_fit as u8,
            ]);
        }
        if let Some(base) = &self.neighbor_base {
            h.update(b"nbase");
            h.update(base.as_bytes());
        }
        if let Some(f) = &self.min_zoom_field {
            h.update(b"mzf");
            h.update(f.as_bytes());
        }
        if let Some(z) = self.min_zoom {
            h.update(b"minz");
            h.update(&[z]);
        }
        if let Some(z) = self.max_zoom {
            h.update(b"maxz");
            h.update(&[z]);
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
        build_text_node(fields, ctx, Stage::Whole)
    }
    fn schema(&self) -> Value {
        text_schema(Stage::Whole)
    }
}

/// `text-labels` — the same node, stopping at the candidates so a shared
/// `label-placement` node can decide them against every other label layer.
pub(super) struct TextLabelsFactory;
impl NodeFactory for TextLabelsFactory {
    fn op_name(&self) -> &'static str {
        "text-labels"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        build_text_node(fields, ctx, Stage::Labels)
    }
    fn schema(&self) -> Value {
        text_schema(Stage::Labels)
    }
}

/// Build the [`IconConfig`] from a node's `icon-*` fields, or `None` when the
/// node names no icon. `icon-sprite` plus either `icon-name` (constant) or
/// `icon-name-expr` (MapLibre data-driven `icon-image`) turn it on.
fn build_icon_config(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
) -> Result<Option<IconConfig>, FactoryError> {
    let name = read_optional_string(fields, "icon-name")?;
    let (name_expr, name_expr_src) = match fields.get("icon-name-expr") {
        Some(v) => {
            let expr = maplibre_expr::parse(v).map_err(|e| FactoryError::BadField {
                field: "icon-name-expr".into(),
                msg: e.to_string(),
            })?;
            // Prefer a String check (icon names stringify); fall back to
            // untyped, since `match`/`step` over literals may not narrow.
            let expr = maplibre_expr::typecheck(&expr, Some(&maplibre_expr::Type::String), false)
                .or_else(|_| maplibre_expr::typecheck(&expr, None, false))
                .map_err(|e| FactoryError::BadField {
                    field: "icon-name-expr".into(),
                    msg: e.to_string(),
                })?;
            (Some(expr), Some(v.to_string()))
        }
        None => (None, None),
    };
    if name.is_none() && name_expr.is_none() {
        return Ok(None);
    }
    let sprite = fields
        .get("icon-sprite")
        .and_then(Value::as_str)
        .ok_or_else(|| FactoryError::MissingField("icon-sprite".into()))?;
    let sprite = super::stamp::resolve_sprite_atlas(sprite, ctx)?;

    let anchor_s = read_string_or(fields, "icon-anchor", ctx, "center")?;
    let anchor = Anchor::parse(&anchor_s).ok_or_else(|| FactoryError::BadField {
        field: "icon-anchor".into(),
        msg: format!("unknown anchor `{anchor_s}`"),
    })?;
    let (size_expr, size_expr_src) =
        parse_expr_field(fields, "icon-size-expr", &maplibre_expr::Type::Number)?;
    let (rotate_expr, rotate_expr_src) =
        parse_expr_field(fields, "icon-rotate-deg-expr", &maplibre_expr::Type::Number)?;
    let (opacity_expr, opacity_expr_src) =
        parse_expr_field(fields, "icon-opacity-expr", &maplibre_expr::Type::Number)?;
    let (padding_expr, padding_expr_src) =
        parse_expr_field(fields, "icon-padding-expr", &maplibre_expr::Type::Number)?;
    let fit_s = read_string_or(fields, "icon-text-fit", ctx, "none")?;
    let text_fit = IconTextFit::parse(&fit_s).ok_or_else(|| FactoryError::BadField {
        field: "icon-text-fit".into(),
        msg: format!("unknown fit `{fit_s}`"),
    })?;
    let text_fit_padding = match fields.get("icon-text-fit-padding") {
        None => [0.0; 4],
        Some(_) => {
            let v = crate::nodes::common::resolve_field(fields, "icon-text-fit-padding", ctx)?;
            let arr =
                v.as_array()
                    .filter(|a| a.len() == 4)
                    .ok_or_else(|| FactoryError::BadField {
                        field: "icon-text-fit-padding".into(),
                        msg: "expected [top, right, bottom, left]".into(),
                    })?;
            let mut out = [0.0f32; 4];
            for (o, v) in out.iter_mut().zip(arr) {
                *o = v.as_f64().unwrap_or(0.0) as f32;
            }
            out
        }
    };
    Ok(Some(IconConfig {
        sprite,
        name,
        name_expr,
        name_expr_src,
        size: read_number_or(fields, "icon-size", ctx, 1.0)? as f32,
        size_expr,
        size_expr_src,
        rotate_deg: read_number_or(fields, "icon-rotate-deg", ctx, 0.0)? as f32,
        rotate_expr,
        rotate_expr_src,
        opacity: read_number_or(fields, "icon-opacity", ctx, 1.0)? as f32,
        opacity_expr,
        opacity_expr_src,
        anchor,
        offset: read_xy(fields, "icon-offset", ctx, [0.0, 0.0])?,
        // MapLibre's `icon-padding` default.
        padding_px: read_number_or(fields, "icon-padding-px", ctx, 2.0)? as f32,
        padding_expr,
        padding_expr_src,
        allow_overlap: read_bool_or(fields, "icon-allow-overlap", ctx, false)?,
        ignore_placement: read_bool_or(fields, "icon-ignore-placement", ctx, false)?,
        optional: read_bool_or(fields, "icon-optional", ctx, false)?,
        text_fit,
        text_fit_padding,
    }))
}

/// Build a [`TextNode`] for either stage: the fields, ports and connections
/// are identical, only the output differs.
fn build_text_node(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
    stage: Stage,
) -> Result<BuiltNode, FactoryError> {
    let features = take_input_ref(fields, "features")?;

    // MapLibre's `icon-image` half. A symbol may be icon-only, so its
    // presence makes `font` / `text` optional.
    let icon = build_icon_config(fields, ctx)?;

    // `font`: an ordered array of `font` / `glyphs` source names —
    // the fallback stack. Each resolves to its source's asset key
    // (a font's `url`; a glyphs source's `{range}` URL template).
    let empty_font = Value::Array(Vec::new());
    let font_field = match (fields.get("font"), &icon) {
        (Some(v), _) => v,
        (None, Some(_)) => &empty_font,
        (None, None) => return Err(FactoryError::MissingField("font".into())),
    };
    let names = font_field
        .as_array()
        .ok_or_else(|| FactoryError::BadField {
            field: "font".into(),
            msg: "expected an array of font source names".into(),
        })?;
    if names.is_empty() && icon.is_none() {
        return Err(FactoryError::BadField {
            field: "font".into(),
            msg: "font stack must name at least one font source".into(),
        });
    }
    // Resolve one `font`/`glyphs` source name to its asset key, validating
    // the source exists and is a font-like source. Shared by the default
    // `font` stack and every `font-stacks` registry entry.
    let resolve_font_source = |field: &str, name: &str| -> Result<String, FactoryError> {
        match ctx.sources.get(name) {
            Some(ezu_style::SourceDecl::Font(f)) => Ok(f.url.clone()),
            Some(ezu_style::SourceDecl::Glyphs(g)) => Ok(g.asset_key()),
            Some(_) => Err(FactoryError::BadField {
                field: field.into(),
                msg: format!("source `{name}` is not a font or glyphs source"),
            }),
            None => Err(FactoryError::UnknownAsset(name.to_string())),
        }
    };
    let mut font_keys = Vec::with_capacity(names.len());
    for v in names {
        let name = v.as_str().ok_or_else(|| FactoryError::BadField {
            field: "font".into(),
            msg: "font stack entries must be strings".into(),
        })?;
        font_keys.push(resolve_font_source("font", name)?);
    }

    // (A) `font-expr`: a MapLibre expression → array<string> of font names,
    // evaluated per feature group. Prefer an `array<string>` check; fall
    // back to untyped (a `case`/`match` over `["literal", [...]]` may not
    // type-narrow), mirroring how `text` typechecks.
    let (font_expr, font_expr_src) = match fields.get("font-expr") {
        Some(v) => {
            let expr = maplibre_expr::parse(v).map_err(|e| FactoryError::BadField {
                field: "font-expr".into(),
                msg: e.to_string(),
            })?;
            let array_of_string =
                maplibre_expr::Type::Array(Box::new(maplibre_expr::Type::String), None);
            let expr = maplibre_expr::typecheck(&expr, Some(&array_of_string), false)
                .or_else(|_| maplibre_expr::typecheck(&expr, None, false))
                .map_err(|e| FactoryError::BadField {
                    field: "font-expr".into(),
                    msg: e.to_string(),
                })?;
            (Some(expr), Some(v.to_string()))
        }
        None => (None, None),
    };

    // (A+B) `font-stacks`: canonical stack key → ordered source names. Each
    // entry resolves exactly like `font`. Insertion order (serde_json
    // `preserve_order`, the workspace default) fixes each stack's `font_id`.
    let font_stacks: Vec<(String, Vec<String>)> = match fields.get("font-stacks") {
        None => Vec::new(),
        Some(Value::Object(map)) => {
            let mut out = Vec::with_capacity(map.len());
            for (key, names) in map {
                let arr = names.as_array().ok_or_else(|| FactoryError::BadField {
                    field: "font-stacks".into(),
                    msg: format!("stack `{key}` must be an array of source names"),
                })?;
                let mut keys = Vec::with_capacity(arr.len());
                for v in arr {
                    let name = v.as_str().ok_or_else(|| FactoryError::BadField {
                        field: "font-stacks".into(),
                        msg: format!("stack `{key}` entries must be strings"),
                    })?;
                    keys.push(resolve_font_source("font-stacks", name)?);
                }
                out.push((key.clone(), keys));
            }
            out
        }
        Some(_) => {
            return Err(FactoryError::BadField {
                field: "font-stacks".into(),
                msg: "expected an object of stack key → source-name array".into(),
            })
        }
    };

    // `text`: a literal string, or a raw MapLibre expression. We prefer a
    // String-typed check (with top-level coercion, so number / property
    // expressions stringify), but a `format` expression yields `formatted`
    // — which String coercion rejects. Fall back to a `Formatted` check
    // (then an untyped one) and flatten the sections at eval time via
    // `label_text`, so real-world `text-field`s (e.g. Protomaps' multi-
    // script `format` labels) build and render instead of erroring.
    let (text, text_expr, text_expr_src) = match fields.get("text") {
        // An icon-only symbol has no label at all; otherwise the field is
        // what the node exists for.
        None if icon.is_some() => (None, None, None),
        None => return Err(FactoryError::MissingField("text".into())),
        Some(Value::String(s)) => (Some(s.clone()), None, None),
        Some(v) => {
            let expr = maplibre_expr::parse(v).map_err(|e| FactoryError::BadField {
                field: "text".into(),
                msg: e.to_string(),
            })?;
            let expr = maplibre_expr::typecheck(&expr, Some(&maplibre_expr::Type::String), true)
                .or_else(|_| {
                    maplibre_expr::typecheck(&expr, Some(&maplibre_expr::Type::Formatted), false)
                })
                .or_else(|_| maplibre_expr::typecheck(&expr, None, false))
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

    // Placement (point / line / line-center) and its line-only knobs.
    let placement_s = read_string_or(fields, "placement", ctx, "point")?;
    let placement = Placement::parse(&placement_s).ok_or_else(|| FactoryError::BadField {
        field: "placement".into(),
        msg: format!("unknown placement `{placement_s}` (point|line|line-center)"),
    })?;
    let spacing_px = read_number_or(fields, "spacing-px", ctx, 250.0)? as f32;
    let max_angle_deg = read_number_or(fields, "max-angle-deg", ctx, 45.0)? as f32;
    let keep_upright = read_bool_or(fields, "keep-upright", ctx, true)?;

    // Layout constants. Enumerated strings are validated here so a
    // typo fails the build instead of silently defaulting.
    let anchor_s = read_string_or(fields, "anchor", ctx, "center")?;
    let anchor = Anchor::parse(&anchor_s).ok_or_else(|| FactoryError::BadField {
        field: "anchor".into(),
        msg: format!("unknown anchor `{anchor_s}`"),
    })?;
    // `anchor-variants` (MapLibre `text-variable-anchor`): an ordered list
    // of anchors tried on collision. Absent → the fixed `anchor`.
    let anchor_variants = match fields.get("anchor-variants") {
        Some(v) => {
            let arr = v.as_array().ok_or_else(|| FactoryError::BadField {
                field: "anchor-variants".into(),
                msg: "expected an array of anchor names".into(),
            })?;
            arr.iter()
                .map(|a| {
                    let s = a.as_str().ok_or_else(|| FactoryError::BadField {
                        field: "anchor-variants".into(),
                        msg: "anchor names must be strings".into(),
                    })?;
                    Anchor::parse(s).ok_or_else(|| FactoryError::BadField {
                        field: "anchor-variants".into(),
                        msg: format!("unknown anchor `{s}`"),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        None => Vec::new(),
    };
    let radial_offset_em = read_number_or(fields, "radial-offset", ctx, 0.0)? as f32;
    let justify_s = read_string_or(fields, "justify", ctx, "auto")?;
    let justify = Justify::parse(&justify_s).ok_or_else(|| FactoryError::BadField {
        field: "justify".into(),
        msg: format!("unknown justify `{justify_s}` (auto|left|center|right)"),
    })?;
    let transform_s = read_string_or(fields, "transform", ctx, "none")?;
    let transform = TextTransform::parse(&transform_s).ok_or_else(|| FactoryError::BadField {
        field: "transform".into(),
        msg: format!("unknown transform `{transform_s}` (none|uppercase|lowercase)"),
    })?;
    let offset_em = read_xy(fields, "offset-em", ctx, [0.0, 0.0])?;
    let max_width_em = read_number_or(fields, "max-width-em", ctx, 10.0)? as f32;
    let line_height = read_number_or(fields, "line-height", ctx, 1.2)? as f32;
    let letter_spacing_em = read_number_or(fields, "letter-spacing-em", ctx, 0.0)? as f32;
    let max_extent_px = read_number_or(fields, "max-extent-px", ctx, 128.0)? as f32;
    // Route outline glyphs through the SDF path for maplibre-gl-js
    // parity (it renders every glyph from an SDF). Default on.
    let outline_sdf = read_bool_or(fields, "outline-sdf", ctx, true)?;

    // Collision. Default on (MapLibre's default); `collide: false`
    // restores the draw-everything behaviour.
    let collide = read_bool_or(fields, "collide", ctx, true)?;
    let text_optional = read_bool_or(fields, "text-optional", ctx, false)?;
    let allow_overlap = read_bool_or(fields, "allow-overlap", ctx, false)?;
    let ignore_placement = read_bool_or(fields, "ignore-placement", ctx, false)?;
    let padding_px = read_number_or(fields, "padding-px", ctx, 2.0)? as f32;
    let (padding_expr, padding_expr_src) =
        parse_expr_field(fields, "padding-expr", &maplibre_expr::Type::Number)?;
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
    let min_zoom = read_optional_zoom(fields, "min-zoom")?;
    let max_zoom = read_optional_zoom(fields, "max-zoom")?;

    // A line-placed icon has no ezu equivalent (MapLibre repeats it along the
    // path); the line path draws the label only.
    let icon = match (placement, icon) {
        (Placement::Point, icon) => icon,
        (_, Some(_)) => {
            tracing::warn!("text: `icon-name` on line placement is not supported — icon ignored");
            None
        }
        (_, None) => None,
    };

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
            stage,
            font_keys,
            text,
            text_expr,
            font_expr,
            font_expr_src,
            font_stacks,
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
            icon,
            text_optional,
            placement,
            spacing_px,
            max_angle_deg,
            keep_upright,
            anchor,
            anchor_variants,
            radial_offset_em,
            justify,
            transform,
            offset_em,
            max_width_em,
            line_height,
            letter_spacing_em,
            max_extent_px,
            outline_sdf,
            collide,
            allow_overlap,
            ignore_placement,
            padding_px,
            padding_expr,
            padding_expr_src,
            sort_key_expr,
            sort_key_expr_src,
            neighbor_base,
            filter_expr,
            filter_expr_src,
            min_zoom_field,
            min_zoom,
            max_zoom,
            ports,
            param_refs: parts.param_refs,
        }),
        connections,
    })
}

/// The document schema for both text ops: identical fields, different
/// output — `text` draws a raster, `text-labels` emits candidates for a
/// shared `label-placement` node.
fn text_schema(stage: Stage) -> Value {
    let mut schema = serde_json::json!({
            "description": "Text labels (MapLibre `symbol-placement`): `placement: point` (default) labels each feature point, `line` / `line-center` walk each polyline with tangent-rotated glyphs. `font` is an ordered fallback stack of `font` and/or `glyphs` source names; `text` is a literal string or a MapLibre string expression evaluated per feature group. Paint properties have optional `*-expr` siblings; layout knobs are build-time constants in em. Collision is on by default and is deterministic across tiles: candidates come from this tile plus the 8 neighbour tiles (host-bound under `<source>.<layer>@dx,dy`), so borders stay seamless. Set `source`/`layer` (the upstream feature source) to enable neighbour gathering; without them collision is centre-tile-only. `icon-name` / `icon-name-expr` add MapLibre's `icon-image` half: the icon is placed together with the text against the same index, so a symbol shows both or neither unless `icon-optional` / `text-optional` say otherwise. `font` and `text` are required unless the symbol is icon-only.",
            "properties": {
                "features": schema_frag::node_ref(),
                "font": { "type": "array", "items": { "type": "string" },
                          "description": "Ordered fallback stack of `font` and/or `glyphs` source names from the document's `sources`. Outline fonts and SDF glyph stacks mix freely; the first entry covering a char shapes it. Also the default/fallback stack for `font-expr` and unlisted `font-stacks` keys." },
                "font-expr": {
                    "description": "A MapLibre expression yielding an array of font names, evaluated per feature group (MapLibre data-driven `text-font`); overrides the constant `font`. Each result is canonicalized (names joined with `,`) and looked up in `font-stacks`; an unlisted stack, a non-array result, or an eval error falls back to `font`. Every stack a host may need must be enumerable in `font-stacks` (wasm hosts pre-bind glyph assets).",
                },
                "font-stacks": { "type": "object",
                                 "additionalProperties": { "type": "array", "items": { "type": "string" } },
                                 "description": "Named dynamic font stacks: canonical stack key (font names joined with `,`) → ordered `font`/`glyphs` source names. Consulted by `font-expr` and by `format` text sections carrying `text-font`. Declaration order fixes each stack's stable id (used by the layout cache and cross-tile collision)." },
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
                "placement": { "type": "string", "enum": ["point", "line", "line-center"],
                               "description": "MapLibre `symbol-placement`. `point` (default) labels each feature point. `line` repeats labels along each polyline every `spacing-px`; `line-center` places one at each line's arc-length midpoint. Line placement ignores wrapping (`max-width-em`) and lays out a single line along the path." },
                "spacing-px": { "type": "number", "minimum": 1.0,
                                "description": "Gap in px between successive line anchors (MapLibre `symbol-spacing`). Line placement only. Default 250." },
                "max-angle-deg": { "type": "number", "minimum": 0.0,
                                   "description": "Max cumulative bend in degrees the line may turn over a label-length window before the anchor is rejected (MapLibre `text-max-angle`). Line placement only. Default 45." },
                "keep-upright": { "type": "boolean",
                                  "description": "Flip a right-to-left label so it reads upright (MapLibre `text-keep-upright`). Line placement only. Default true." },
                "anchor": { "type": "string", "enum": ["center", "left", "right", "top", "bottom", "top-left", "top-right", "bottom-left", "bottom-right"],
                            "description": "Which part of the label block sits on the point (point placement). Default `center`." },
                "anchor-variants": { "type": "array", "items": { "type": "string", "enum": ["center", "left", "right", "top", "bottom", "top-left", "top-right", "bottom-left", "bottom-right"] },
                            "description": "MapLibre `text-variable-anchor`: anchors tried in order on collision, the first free one placed (point placement). Overrides `anchor` when set." },
                "radial-offset": { "type": "number",
                            "description": "MapLibre `text-radial-offset`: em distance each `anchor-variants` entry pushes the block away from the point in its direction. Default 0." },
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
                "outline-sdf": { "type": "boolean",
                                 "description": "Render outline-font glyphs through the SDF field-sampling path (maplibre-gl-js style, which draws every glyph from an SDF) instead of a per-glyph vector fill / stroke. The halo then comes from a distance threshold rather than a stroke, matching MapLibre's halo shape and cost. SDF glyph (`glyphs`) stacks are unaffected. Default true." },
                "collide": { "type": "boolean",
                             "description": "Whether to run deterministic label collision. Default true (MapLibre's default). Set false to draw every label (the pre-collision behaviour)." },
                "allow-overlap": { "type": "boolean",
                                   "description": "MapLibre `text-allow-overlap`: place the label even if it collides. It still reserves its box (blocking later labels) unless `ignore-placement`. Default false." },
                "ignore-placement": { "type": "boolean",
                                      "description": "MapLibre `text-ignore-placement`: don't let this label block later ones (skip inserting its collision box). Default false." },
                "padding-px": { "type": "number", "minimum": 0.0,
                                "description": "Collision-box inflation in px on every side. Default 2." },
                "padding-expr": {
                    "description": "A MapLibre number expression (MapLibre `text-padding`), evaluated per feature group; overrides the constant `padding-px` for that group's collision boxes.",
                },
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
                "min-zoom": { "type": "integer", "minimum": 0, "maximum": 24,
                              "description": "Style layer zoom gate: draw nothing below this zoom (mirrors the `features` node)." },
                "max-zoom": { "type": "integer", "minimum": 0, "maximum": 24,
                              "description": "Style layer zoom gate: draw nothing above this zoom (mirrors the `features` node)." },
                "icon-sprite": schema_frag::asset_ref(),
                "icon-name": { "type": "string",
                               "description": "Constant MapLibre `icon-image`: the sprite to draw at each point, cropped from `icon-sprite`. Needs `icon-sprite`." },
                "icon-name-expr": {
                    "description": "A MapLibre expression naming each feature's icon (data-driven `icon-image`), cropped from `icon-sprite`; overrides `icon-name`. A group resolving to an empty/unknown name draws no icon and places as a text-only symbol.",
                },
                "icon-size": { "type": "number", "minimum": 0.0,
                               "description": "MapLibre `icon-size`: multiplier on the sprite's authored display size (atlas pixels over its pixel ratio). Default 1." },
                "icon-size-expr": { "description": "A MapLibre number expression, evaluated per feature group; overrides `icon-size`." },
                "icon-anchor": { "type": "string", "enum": ["center", "left", "right", "top", "bottom", "top-left", "top-right", "bottom-left", "bottom-right"],
                                 "description": "Which part of the icon sits on the point (MapLibre `icon-anchor`). Default `center`." },
                "icon-offset": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2,
                                 "description": "MapLibre `icon-offset`: shift [x, y] in px, itself scaled by `icon-size`. Default [0, 0]." },
                "icon-rotate-deg": { "type": "number",
                                     "description": "MapLibre `icon-rotate`: rotation in degrees clockwise about the icon centre. Default 0." },
                "icon-rotate-deg-expr": { "description": "A MapLibre number expression (degrees clockwise); overrides `icon-rotate-deg`." },
                "icon-opacity": schema_frag::unit_number(),
                "icon-opacity-expr": { "description": "A MapLibre number expression giving the icon's opacity; overrides `icon-opacity`." },
                "icon-padding-px": { "type": "number", "minimum": 0.0,
                                     "description": "MapLibre `icon-padding`: collision-box inflation around the icon, in px. Default 2." },
                "icon-padding-expr": { "description": "A MapLibre number expression (px), evaluated per feature group; overrides `icon-padding-px`." },
                "icon-allow-overlap": { "type": "boolean",
                                        "description": "MapLibre `icon-allow-overlap`. A symbol carries one overlap decision, so with text present this and `allow-overlap` must both be set for the symbol to ignore collision. Default false." },
                "icon-ignore-placement": { "type": "boolean",
                                           "description": "MapLibre `icon-ignore-placement`; with text present it pairs with `ignore-placement` the same way as the overlap flags. Default false." },
                "icon-optional": { "type": "boolean",
                                   "description": "MapLibre `icon-optional`: let the text place when the icon's box is blocked. Default false — icon and text place as one unit." },
                "text-optional": { "type": "boolean",
                                   "description": "MapLibre `text-optional`: let the icon place when the text's box is blocked. Default false." },
                "icon-text-fit": { "enum": ["none", "width", "height", "both"],
                                   "description": "MapLibre `icon-text-fit`: stretch the icon to the box its label occupies, on the named axes. The sprite's `stretchX`/`stretchY`/`content` metadata decides which bands absorb the growth, so a shield keeps its corners; a sprite without it scales whole. The fitted box is also the icon's collision box. Default `none`." },
                "icon-text-fit-padding": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4,
                                           "description": "MapLibre `icon-text-fit-padding`: `[top, right, bottom, left]` px added around the label's box before the icon is fitted to it. Default [0, 0, 0, 0]." },
            },
        "required": ["features"],
    });
    if stage == Stage::Labels {
        schema["description"] = Value::String(format!(
            "{} Emits placement candidates instead of pixels: wire this node into a \
             `label-placement` node and draw the result with `text-draw`.",
            schema["description"].as_str().unwrap_or_default(),
        ));
    }
    schema
}

ezu_graph::submit_node!(TextFactory);
ezu_graph::submit_node!(TextLabelsFactory);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_offset_mirrors_per_anchor() {
        // A `text-offset` of [1.1, 0] pushes the label right of a `left`-anchored
        // point and left of a `right`-anchored one, so both variants clear the
        // symbol instead of stacking on the same side.
        let o = [1.1, 0.0];
        assert_eq!(anchor_offset_em(Anchor::Left, o, 0.0), [1.1, 0.0]);
        assert_eq!(anchor_offset_em(Anchor::Right, o, 0.0), [-1.1, 0.0]);
        // A negative offset has the same magnitude either way.
        assert_eq!(anchor_offset_em(Anchor::Left, [-1.1, 0.0], 0.0), [1.1, 0.0]);
        // An axis the anchor does not name contributes nothing.
        assert_eq!(
            anchor_offset_em(Anchor::Center, [1.1, 2.0], 0.0),
            [0.0, 0.0]
        );
        // Vertical anchors lose the baseline gap.
        let [_, y] = anchor_offset_em(Anchor::Top, [0.0, 1.0], 0.0);
        assert!((y - (1.0 - BASELINE_OFFSET_EM)).abs() < 1e-6);
        let [_, y] = anchor_offset_em(Anchor::Bottom, [0.0, 1.0], 0.0);
        assert!((y + (1.0 - BASELINE_OFFSET_EM)).abs() < 1e-6);
    }

    #[test]
    fn radial_offset_pushes_away_from_the_point() {
        // `text-radial-offset` wins over `text-offset` and splits corners.
        let r = 2.0f32;
        assert_eq!(anchor_offset_em(Anchor::Left, [9.0, 9.0], r), [r, 0.0]);
        assert_eq!(anchor_offset_em(Anchor::Right, [9.0, 9.0], r), [-r, 0.0]);
        let [x, y] = anchor_offset_em(Anchor::TopLeft, [0.0, 0.0], r);
        let diag = r / std::f32::consts::SQRT_2;
        assert!((x - diag).abs() < 1e-6);
        assert!((y - (diag - BASELINE_OFFSET_EM)).abs() < 1e-6);
    }
}
