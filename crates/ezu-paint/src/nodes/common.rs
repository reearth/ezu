//! Shared helpers for built-in node implementations.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use ezu_core::TileId as CoreTileId;
use ezu_graph::{
    Asset, EnumTable, EvalCtx, EvalError, FactoryCtx, FactoryError, InEnum, InReader, PortKind,
    PortValue, RasterBuf, ScalarField,
};
use ezu_style as spec;
use hokusai::Brush;
use serde_json::Value;

use crate::render::{collect_groups, SharedLayer};
use crate::Canvas;

// ---------------------------------------------------------------------------
// Concrete payload types for type-erased ports.

/// One feature's properties paired with its own geometry. A
/// [`FilteredFeatures`] payload is nothing but an ordered list of these — the
/// single source of truth for a `Features` port.
///
/// `properties` are pre-converted (once, at collection time) into the
/// `maplibre-expr` value form and shared behind an `Arc`, so cloning a group —
/// e.g. propagating it through a geometry transform — is a refcount bump, not
/// a deep copy. Data-driven paint nodes evaluate a per-feature expression
/// against these properties (see `group_expr_context` in the crate's `render` module).
///
/// Synthetic geometry (e.g. `literal-geometry`) and merge-style geometry
/// transforms (e.g. `convex-hull`) emit a single group with empty properties.
pub struct FeatureGroup {
    pub properties: Arc<BTreeMap<String, maplibre_expr::Value>>,
    pub polygons: Vec<ezu_features::Polygon>,
    pub lines: Vec<Vec<(i32, i32)>>,
    pub points: Vec<(i32, i32)>,
}

impl FeatureGroup {
    /// A group with no per-feature properties, for synthetic geometry sources
    /// and geometry transforms that merge geometry across features.
    pub fn synthetic(
        polygons: Vec<ezu_features::Polygon>,
        lines: Vec<Vec<(i32, i32)>>,
        points: Vec<(i32, i32)>,
    ) -> FeatureGroup {
        FeatureGroup {
            properties: Arc::new(BTreeMap::new()),
            polygons,
            lines,
            points,
        }
    }
}

/// Payload carried on a `Features` port. Produced by `features` and the
/// synthetic geometry sources; consumed by every paint and geometry node.
///
/// `groups` is the single source of truth: an ordered list of per-feature
/// [`FeatureGroup`]s. Consumers that want the old flat view — the
/// concatenation of every group's geometry, in group order — walk it via the
/// [`points`](Self::points) / [`lines`](Self::lines) /
/// [`polygons`](Self::polygons) iterators (or collect locally where a
/// contiguous `Vec` is genuinely required).
///
/// [`neighbours`](Self::neighbours) is empty unless a node downstream asked
/// for the neighbour tiles' copies (see [`ezu_graph::Node::wants_neighbours`]).
/// They are kept apart from `groups` so that every op drawing or exporting
/// features keeps working from this tile's own geometry alone.
#[non_exhaustive]
pub struct FilteredFeatures {
    /// The units one tile spans across. `0` when this tile has no copy of
    /// the layer at all, as a vector tile leaves out a layer with nothing
    /// in it; the neighbours then carry their own.
    pub extent: u32,
    pub groups: Vec<FeatureGroup>,
    /// How many zoom levels the geometry was scaled up from the tile it was
    /// encoded for, when it came straight from an overzoomed source tile
    /// (see [`SharedLayer::overzoom`]); 0 otherwise.
    pub overzoom: u8,
    /// The neighbour tiles' copies, when something downstream asked for
    /// them: each in its own tile's frame, processed as that tile's own
    /// render would process it. Row by row from the north-west, as
    /// [`ezu_graph::neighbor_bindings`] orders them; a neighbour the host
    /// did not provide is absent.
    pub neighbours: Vec<NeighbourLayer>,
}

/// One neighbour tile's features, carried in [`FilteredFeatures::neighbours`].
#[non_exhaustive]
pub struct NeighbourLayer {
    /// Tile offset from this tile: `+x` east, `+y` south, each in `-1..=1`.
    pub dx: i8,
    pub dy: i8,
    /// The neighbour layer's [`FilteredFeatures::overzoom`].
    pub overzoom: u8,
    /// The units the neighbour tile spans across; its geometry lies in
    /// `[0, extent]` like a tile's own.
    pub extent: u32,
    pub groups: Vec<FeatureGroup>,
}

impl NeighbourLayer {
    /// The neighbour `(dx, dy)` tiles away, its `groups` in its own
    /// `[0, extent]` frame. `overzoom` starts at 0.
    pub fn new(dx: i8, dy: i8, extent: u32, groups: Vec<FeatureGroup>) -> Self {
        Self {
            dx,
            dy,
            overzoom: 0,
            extent,
            groups,
        }
    }

    /// The same neighbour, scaled up `overzoom` zoom levels from the tile
    /// it was encoded for.
    pub fn with_overzoom(mut self, overzoom: u8) -> Self {
        self.overzoom = overzoom;
        self
    }
}

impl FilteredFeatures {
    /// This tile's `groups` in a `[0, extent]` frame, with no overzoom and
    /// no neighbours.
    pub fn new(extent: u32, groups: Vec<FeatureGroup>) -> Self {
        Self {
            extent,
            groups,
            overzoom: 0,
            neighbours: Vec::new(),
        }
    }

    /// The same features, scaled up `overzoom` zoom levels from the tile
    /// they were encoded for.
    pub fn with_overzoom(mut self, overzoom: u8) -> Self {
        self.overzoom = overzoom;
        self
    }

    /// The same features, carrying the neighbour tiles' copies.
    pub fn with_neighbours(mut self, neighbours: Vec<NeighbourLayer>) -> Self {
        self.neighbours = neighbours;
        self
    }

    /// Every group's points, in group order (= the old flat concatenation).
    pub fn points(&self) -> impl Iterator<Item = (i32, i32)> + '_ {
        self.groups.iter().flat_map(|g| g.points.iter().copied())
    }

    /// Every group's polylines, in group order.
    pub fn lines(&self) -> impl Iterator<Item = &Vec<(i32, i32)>> + '_ {
        self.groups.iter().flat_map(|g| g.lines.iter())
    }

    /// Every group's polygons, in group order.
    pub fn polygons(&self) -> impl Iterator<Item = &ezu_features::Polygon> + '_ {
        self.groups.iter().flat_map(|g| g.polygons.iter())
    }

    /// Whether any group carries at least one point.
    pub fn has_points(&self) -> bool {
        self.groups.iter().any(|g| !g.points.is_empty())
    }

    /// Whether any group carries at least one polyline.
    pub fn has_lines(&self) -> bool {
        self.groups.iter().any(|g| !g.lines.is_empty())
    }

    /// Whether any group carries at least one polygon.
    pub fn has_polygons(&self) -> bool {
        self.groups.iter().any(|g| !g.polygons.is_empty())
    }
}

/// Payload carried on a `Brush` port. Wraps a hokusai brush.
pub type BrushPayload = Brush;

// ---------------------------------------------------------------------------
// Field reading

/// Resolve a node field: if it's a string starting with `$`, look it up
/// in the document's `params`. Returns the resolved JSON value.
///
/// The value resolved here is the param's **declared default**, baked in
/// when the graph is built — a caller's render-time override does not
/// reach it. That is the right behaviour for a field the graph has to
/// know before it can evaluate anything (a layout constant, a generator
/// kind), and a trap everywhere else, so it says so out loud. Fields
/// that should follow a param per render read it through
/// [`ezu_graph::InReader`] instead, which keeps it as an `In<T>`.
pub(super) fn resolve_field(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    ctx: &FactoryCtx<'_>,
) -> Result<Value, FactoryError> {
    let v = fields
        .get(name)
        .ok_or_else(|| FactoryError::MissingField(name.to_string()))?;
    if let Some(s) = v.as_str() {
        match spec::FieldRef::classify(s) {
            spec::FieldRef::Param(p) => {
                let decl = ctx
                    .params
                    .get(p)
                    .ok_or_else(|| FactoryError::UnknownParam(p.to_string()))?;
                ctx.warn(format!(
                    "field `{name}`: `${p}` is read when the graph is built, so its \
                     declared default ({}) is baked in and render-time overrides of \
                     `{p}` will not change this field",
                    decl.default,
                ));
                return Ok(decl.default.clone());
            }
            spec::FieldRef::Node(_) => {
                return Err(FactoryError::BadField {
                    field: name.into(),
                    msg: "expected literal or $param, got @node-ref".into(),
                });
            }
            spec::FieldRef::Literal(_) => {}
        }
    }
    Ok(v.clone())
}

pub(super) fn read_number(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    ctx: &FactoryCtx<'_>,
) -> Result<f64, FactoryError> {
    let v = resolve_field(fields, name, ctx)?;
    v.as_f64().ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: "expected number".into(),
    })
}

pub(super) fn read_number_or(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    ctx: &FactoryCtx<'_>,
    default: f64,
) -> Result<f64, FactoryError> {
    if !fields.contains_key(name) {
        return Ok(default);
    }
    read_number(fields, name, ctx)
}

pub(super) fn read_bool_or(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    ctx: &FactoryCtx<'_>,
    default: bool,
) -> Result<bool, FactoryError> {
    if !fields.contains_key(name) {
        return Ok(default);
    }
    let v = resolve_field(fields, name, ctx)?;
    v.as_bool().ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: "expected boolean".into(),
    })
}

/// Read a zoom-level field (`min-zoom` / `max-zoom`), absent → `None`.
pub(super) fn read_optional_zoom(
    fields: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<u8>, FactoryError> {
    let Some(v) = fields.get(key) else {
        return Ok(None);
    };
    let n = v.as_u64().ok_or_else(|| FactoryError::BadField {
        field: key.into(),
        msg: "expected non-negative integer".into(),
    })?;
    if n > 24 {
        return Err(FactoryError::BadField {
            field: key.into(),
            msg: format!("zoom {n} out of range (0..=24)"),
        });
    }
    Ok(Some(n as u8))
}

/// How to read samples outside the source raster's `[0, w) x [0, h)`
/// extent. The upstream `required_pad` should normally keep us inside,
/// so this only kicks in for extreme amplitudes or at the very edge of
/// the world (zoom 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BoundaryMode {
    /// Clamp to the nearest edge pixel.
    Clamp,
    /// Return transparent black for out-of-bounds samples.
    Transparent,
    /// Reflect at edges so the pattern reads `...|abc|cba|abc|...`.
    Mirror,
    /// Repeat the raster so the pattern reads `...|abc|abc|abc|...`:
    /// what leaves one edge comes back in at the opposite one, as a
    /// sprite that repeats edge to edge needs.
    Wrap,
}

/// What a `boundary` field accepts; `repeat` is another name for `wrap`.
pub(super) const BOUNDARIES: EnumTable<BoundaryMode> = &[
    ("clamp", BoundaryMode::Clamp),
    ("transparent", BoundaryMode::Transparent),
    ("mirror", BoundaryMode::Mirror),
    ("wrap", BoundaryMode::Wrap),
    ("repeat", BoundaryMode::Wrap),
];

/// The schema of a `boundary` field read through [`BOUNDARIES`].
pub(super) fn boundary_schema() -> Value {
    ezu_graph::schema_frag::in_enum(serde_json::json!({
        "type": "string",
        "enum": ezu_graph::enum_names(BOUNDARIES),
        "default": "clamp",
        "description": "How to read past the input's edge: `clamp` repeats the edge pixel, `transparent` reads nothing, `mirror` reflects the image, and `wrap` (or `repeat`) tiles it, so what leaves one edge comes back in at the opposite one.",
    }))
}

/// Resolve a possibly out-of-range integer pixel coordinate into a
/// valid `[0, dim)` index, or `None` for `Transparent` out-of-range.
#[inline]
fn wrap_index(i: i64, dim: u32, mode: BoundaryMode) -> Option<u32> {
    let d = dim as i64;
    if d == 0 {
        return None;
    }
    if i >= 0 && i < d {
        return Some(i as u32);
    }
    match mode {
        BoundaryMode::Clamp => Some(i.clamp(0, d - 1) as u32),
        BoundaryMode::Transparent => None,
        BoundaryMode::Mirror => {
            // Period 2*(d-1); reflect.
            if d == 1 {
                return Some(0);
            }
            let period = 2 * (d - 1);
            let mut k = i % period;
            if k < 0 {
                k += period;
            }
            if k >= d {
                k = period - k;
            }
            Some(k as u32)
        }
        BoundaryMode::Wrap => Some(i.rem_euclid(d) as u32),
    }
}

#[inline]
fn read_pixel_or(src: &RasterBuf, ix: i64, iy: i64, mode: BoundaryMode) -> [f32; 4] {
    let Some(x) = wrap_index(ix, src.width, mode) else {
        return [0.0; 4];
    };
    let Some(y) = wrap_index(iy, src.height, mode) else {
        return [0.0; 4];
    };
    let p = src.pixel(x, y);
    [p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32]
}

/// Bilinear sample of a premultiplied RGBA8 raster at floating-point
/// pixel coordinates `(x, y)`. Linear blending of premultiplied values
/// is the correct path — avoids halos near transparent edges.
pub(super) fn sample_bilinear(src: &RasterBuf, x: f64, y: f64, mode: BoundaryMode) -> [u8; 4] {
    let fx = x.floor();
    let fy = y.floor();
    let tx = (x - fx) as f32;
    let ty = (y - fy) as f32;
    let ix = fx as i64;
    let iy = fy as i64;
    let p00 = read_pixel_or(src, ix, iy, mode);
    let p10 = read_pixel_or(src, ix + 1, iy, mode);
    let p01 = read_pixel_or(src, ix, iy + 1, mode);
    let p11 = read_pixel_or(src, ix + 1, iy + 1, mode);
    let mut out = [0u8; 4];
    for c in 0..4 {
        let a = p00[c] + (p10[c] - p00[c]) * tx;
        let b = p01[c] + (p11[c] - p01[c]) * tx;
        let v = a + (b - a) * ty;
        out[c] = v.round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// Coordinate anchor for procedural sources (gradients, noise, etc.).
/// `Tile` = positions are fractions of the current tile (constant
/// output per canvas size). `World` = positions are fractions of the
/// full Mercator world at z=0 (output depends on tile id, so the
/// pattern stays continuous across tile boundaries).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Anchor {
    Tile,
    World,
}

/// Seed a procedural field should use when the style names none.
///
/// Anchoring decides this, because the two anchors want opposite things.
/// A `World` field is one function over the whole map, so every tile has
/// to sample the *same* field — a per-tile seed would line the sampling
/// coordinates up and then swap the field underneath them, which reads as
/// a discontinuity exactly at the border the anchor exists to hide. A
/// `Tile` field is deliberately per-tile, so the host's per-tile
/// `rng_seed` is the right source of variation there.
///
/// The constant is arbitrary; it only has to be stable across tiles and
/// across runs.
pub(super) fn default_field_seed(anchor: Anchor, rng_seed: u64) -> u32 {
    match anchor {
        Anchor::World => 0x5EED_1F1E,
        Anchor::Tile => (rng_seed as u32) ^ ((rng_seed >> 32) as u32),
    }
}

pub(super) const ANCHORS: EnumTable<Anchor> = &[("tile", Anchor::Tile), ("world", Anchor::World)];

/// MapLibre's nine names for the part of a box that sits on a point
/// (`text-anchor`, `icon-anchor`), the names every anchor field takes.
pub(super) const ANCHOR_NAMES: EnumTable<ezu_core::text::Anchor> = {
    use ezu_core::text::Anchor as A;
    &[
        ("center", A::Center),
        ("left", A::Left),
        ("right", A::Right),
        ("top", A::Top),
        ("bottom", A::Bottom),
        ("top-left", A::TopLeft),
        ("top-right", A::TopRight),
        ("bottom-left", A::BottomLeft),
        ("bottom-right", A::BottomRight),
    ]
};

/// Warn about each deprecated name a fixed-set field is given, naming the
/// one to write instead: as a literal, or as the default or an `enum`
/// value of the `$param` it reads. `renamed` pairs each old name with its
/// replacement. The old names still parse, so this changes no output.
pub(super) fn warn_renamed(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
    field: &str,
    renamed: &[(&str, &str)],
) {
    let Some(s) = fields.get(field).and_then(Value::as_str) else {
        return;
    };
    let new_for = |old: &str| renamed.iter().find(|(o, _)| *o == old).map(|(_, n)| *n);
    match spec::FieldRef::classify(s) {
        spec::FieldRef::Literal(old) => {
            if let Some(new) = new_for(old) {
                ctx.warn(format!(
                    "`{field}`: `{old}` is deprecated: write \"{field}\": \"{new}\""
                ));
            }
        }
        spec::FieldRef::Param(p) => {
            let Some(decl) = ctx.params.get(p) else {
                return;
            };
            let mut named: Vec<&str> = decl.default.as_str().into_iter().collect();
            named.extend(decl.choices.iter().flatten().map(String::as_str));
            let mut seen = Vec::new();
            for old in named {
                if let Some(new) = new_for(old) {
                    if !seen.contains(&old) {
                        seen.push(old);
                        ctx.warn(format!(
                            "`{field}`: param `${p}` names `{old}`, which is deprecated: \
                             write `{new}`"
                        ));
                    }
                }
            }
        }
        spec::FieldRef::Node(_) => {}
    }
}

/// The schema of an `anchor` field taking `tile` / `world`.
pub(super) fn anchor_schema(default: &str, description: Option<&str>) -> Value {
    let mut literal = serde_json::json!({
        "type": "string",
        "enum": ["tile", "world"],
        "default": default,
    });
    if let Some(d) = description {
        literal["description"] = d.into();
    }
    ezu_graph::schema_frag::in_enum(literal)
}

pub(super) fn read_xy(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    ctx: &FactoryCtx<'_>,
    default: [f32; 2],
) -> Result<[f32; 2], FactoryError> {
    if !fields.contains_key(name) {
        return Ok(default);
    }
    let v = resolve_field(fields, name, ctx)?;
    let arr = v.as_array().ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: "expected [x, y] array".into(),
    })?;
    if arr.len() != 2 {
        return Err(FactoryError::BadField {
            field: name.into(),
            msg: format!("expected exactly 2 numbers, got {}", arr.len()),
        });
    }
    let x = arr[0].as_f64().ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: "x must be number".into(),
    })? as f32;
    let y = arr[1].as_f64().ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: "y must be number".into(),
    })? as f32;
    Ok([x, y])
}

/// A gradient stop table read from `[[t, "#hex"], ...]`, either half of
/// each pair a literal or a `$param`. Resolve with [`resolve_stops`].
pub(super) type StopsIn = Vec<(ezu_graph::In<f64>, ezu_graph::In<[f32; 4]>)>;

/// Parse a gradient `stops` field: `[[t, "#hex"], ...]` with t normally
/// in [0, 1] and colour an `#rrggbb[aa]` string. Requires at least two
/// stops. Either half may be a `$param`, so ordering is not checked
/// here — [`resolve_stops`] sorts once the values are known.
pub(super) fn read_stops(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    r: &mut ezu_graph::InReader<'_, '_>,
) -> Result<StopsIn, FactoryError> {
    let v = fields
        .get(name)
        .ok_or_else(|| FactoryError::MissingField(name.to_string()))?;
    let arr = v.as_array().ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: "expected array of [t, color] pairs".into(),
    })?;
    if arr.len() < 2 {
        return Err(FactoryError::BadField {
            field: name.into(),
            msg: "gradient needs at least 2 stops".into(),
        });
    }
    let mut out = Vec::with_capacity(arr.len());
    for (i, pt) in arr.iter().enumerate() {
        let pair = pt.as_array().ok_or_else(|| FactoryError::BadField {
            field: name.into(),
            msg: format!("entry {i}: expected [t, color] pair"),
        })?;
        if pair.len() != 2 {
            return Err(FactoryError::BadField {
                field: name.into(),
                msg: format!("entry {i}: expected exactly 2 entries"),
            });
        }
        out.push((
            r.nested(&format!("{name}[{i}][0]"), &pair[0])?,
            r.nested(&format!("{name}[{i}][1]"), &pair[1])?,
        ));
    }
    Ok(out)
}

/// One resolved gradient stop.
pub(super) struct GradientStop {
    t: f32,
    pub(super) rgba: [f32; 4],
    /// `rgba` in the interpolation space, converted once per eval so a pixel
    /// pays only for the interpolation itself.
    converted: [f32; 4],
}

/// Resolve a stop table for one eval and sort it by `t`, which
/// [`sample_stops`] requires. Call once per eval, never per pixel.
pub(super) fn resolve_stops(
    stops: &StopsIn,
    space: crate::color_interp::InterpSpace,
    ctx: &EvalCtx<'_>,
    inputs: &[Option<PortValue>],
) -> Result<Vec<GradientStop>, EvalError> {
    let mut out = Vec::with_capacity(stops.len());
    for (t, c) in stops {
        let rgba = c.get(ctx, inputs)?;
        out.push(GradientStop {
            t: t.get(ctx, inputs)? as f32,
            rgba,
            converted: crate::color_interp::to_space(rgba, space),
        });
    }
    out.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out)
}

/// Feed a stop table's static identity into `Node::param_hash`.
pub(super) fn hash_stops(stops: &StopsIn, h: &mut xxhash_rust::xxh3::Xxh3) {
    for (t, c) in stops {
        t.param_hash(h);
        c.param_hash(h);
    }
}

/// The colour spaces a stop table can interpolate in, with the two other
/// names `hsv` and `hcl` go by.
pub(super) const SPACES: EnumTable<crate::color_interp::InterpSpace> = {
    use crate::color_interp::InterpSpace as S;
    &[
        ("rgb", S::Rgb),
        ("hsl", S::Hsl),
        ("hsv", S::Hsv),
        ("hcl", S::Hcl),
        ("lab", S::Lab),
        ("hsb", S::Hsv),
        ("lch", S::Hcl),
    ]
};

/// The schema of a `space` field read by [`read_space`].
pub(super) fn space_schema(description: &str) -> Value {
    ezu_graph::schema_frag::in_enum(serde_json::json!({
        "type": "string",
        "enum": ["rgb", "hsl", "hsv", "hcl", "lab"],
        "default": "rgb",
        "description": description,
    }))
}

/// Read an optional colour-interpolation `space` field (`"rgb"` default,
/// plus `"hsl"` / `"hsv"` / `"hcl"` / `"lab"`). Used by `color-ramp` and the
/// gradient ops so a stop table can interpolate hue on the shortest path.
///
/// A literal is matched without regard to case, as it always has been; a
/// `string` `$param` must use the lower-case names.
pub(super) fn read_space(
    r: &mut InReader<'_, '_>,
    fields: &serde_json::Map<String, Value>,
) -> Result<InEnum<crate::color_interp::InterpSpace>, FactoryError> {
    use crate::color_interp::InterpSpace;
    match fields.get("space") {
        None | Some(Value::Null) => Ok(InEnum::constant("space", SPACES, InterpSpace::default())),
        Some(Value::String(s)) if !s.starts_with(['$', '@']) => {
            let space = InterpSpace::parse(s).ok_or_else(|| FactoryError::BadField {
                field: "space".into(),
                msg: format!("unknown colour space `{s}` (rgb|hsl|hsv|hcl|lab)"),
            })?;
            Ok(InEnum::constant("space", SPACES, space))
        }
        Some(_) => r.enum_or("space", InterpSpace::default(), SPACES),
    }
}

/// How a gradient colours a position between two stops — the choice
/// `color-ramp`'s `interpolation` offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StopInterpolation {
    /// Blend the two stops' colours in the gradient's `space`.
    Linear,
    /// Take the colour of the stop at or below the position, as MapLibre's
    /// `step` does, so the gradient paints flat bands.
    Step,
}

pub(super) const STOP_INTERPOLATIONS: EnumTable<StopInterpolation> = &[
    ("linear", StopInterpolation::Linear),
    ("step", StopInterpolation::Step),
];

/// The schema of a gradient's `interpolation` field.
pub(super) fn interpolation_schema() -> Value {
    ezu_graph::schema_frag::in_enum(serde_json::json!({
        "type": "string",
        "enum": ["linear", "step"],
        "default": "linear",
        "description": "`linear` (default) blends the colours of the two stops around a position. `step` gives a position the colour of the stop at or below it (the first stop's below them all), as `color-ramp` and MapLibre's `step` do, so the gradient paints flat bands. Ignored with `kind: scalar`.",
    }))
}

/// The colour of the last stop at or below `t`, or the first stop's
/// below them all. Stops must be sorted by ascending `t`.
pub(super) fn step_stops(stops: &[GradientStop], t: f32) -> [f32; 4] {
    match stops.iter().rev().find(|s| s.t <= t).or(stops.first()) {
        Some(s) => s.rgba,
        None => [0.0; 4],
    }
}

/// A gradient's colour at `t`, by `interpolation`.
pub(super) fn color_at(
    stops: &[GradientStop],
    t: f32,
    space: crate::color_interp::InterpSpace,
    interpolation: StopInterpolation,
) -> [f32; 4] {
    match interpolation {
        StopInterpolation::Linear => sample_stops(stops, t, space),
        StopInterpolation::Step => step_stops(stops, t),
    }
}

/// Linearly interpolate gradient stops at parameter `t`, in colour `space`.
/// Stops must be non-empty and sorted by ascending `t`. Out-of-range `t`
/// clamps to the endpoint colors.
pub(super) fn sample_stops(
    stops: &[GradientStop],
    t: f32,
    space: crate::color_interp::InterpSpace,
) -> [f32; 4] {
    if stops.is_empty() {
        return [0.0; 4];
    }
    if t <= stops[0].t {
        return stops[0].rgba;
    }
    let last = stops.last().expect("stops is non-empty (guarded above)");
    if t >= last.t {
        return last.rgba;
    }
    for w in stops.windows(2) {
        if t >= w[0].t && t <= w[1].t {
            let d = w[1].t - w[0].t;
            if d < 1e-6 {
                return w[1].rgba;
            }
            let f = (t - w[0].t) / d;
            return crate::color_interp::interpolate_converted(
                w[0].converted,
                w[1].converted,
                f,
                space,
            );
        }
    }
    last.rgba
}

pub(super) fn read_optional_string(
    fields: &serde_json::Map<String, Value>,
    name: &str,
) -> Result<Option<String>, FactoryError> {
    let Some(v) = fields.get(name) else {
        return Ok(None);
    };
    let s = v.as_str().ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: "expected string".into(),
    })?;
    Ok(Some(s.to_string()))
}

// ---------------------------------------------------------------------------
// Source resolution

/// Resolve the `source` field of a source node (`features`, `raster`,
/// `dem`, …) against the document's `sources` block. `kinds` names the
/// declaration types the node accepts, for error and warning text
/// (e.g. `` "`dem`" ``), and `accepts` decides which ones match.
///
/// Omitting `source` still falls back to the document's single matching
/// source, but that fallback makes the style depend on something it
/// never says, and it silently changes meaning the day a second source
/// is declared — so it comes with a build warning.
pub(super) fn resolve_source(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
    kinds: &str,
    accepts: fn(&spec::SourceDecl) -> bool,
) -> Result<String, FactoryError> {
    if let Some(name) = read_optional_string(fields, "source")? {
        return match ctx.sources.get(&name) {
            Some(decl) if accepts(decl) => Ok(name),
            Some(_) => Err(FactoryError::BadField {
                field: "source".into(),
                msg: format!("`{name}` exists but is not a {kinds} source"),
            }),
            None => Err(FactoryError::BadField {
                field: "source".into(),
                msg: format!("no source named `{name}` in document"),
            }),
        };
    }

    let mut matches = ctx.sources.iter().filter(|(_, decl)| accepts(decl));
    match (matches.next(), matches.next()) {
        (Some((name, _)), None) => {
            ctx.warn(format!(
                "`source` is not set — falling back to the document's only {kinds} source, \
                 `{name}`. Name it: the style changes meaning the day a second one is declared."
            ));
            Ok(name.clone())
        }
        (None, _) => Err(FactoryError::BadField {
            field: "source".into(),
            msg: format!("no {kinds} source in document; declare one or pass `source` explicitly"),
        }),
        (Some(_), Some(_)) => Err(FactoryError::BadField {
            field: "source".into(),
            msg: format!("multiple {kinds} sources in document; pass `source` explicitly"),
        }),
    }
}

/// What a `features` node reads and how it filters it, as a node fed by
/// it needs to know to read the same layer from the neighbour tiles: the
/// `<source>.<layer>` binding, the filter and per-feature zoom field, and
/// the zoom band outside which it yields nothing.
pub(super) struct FeatureOrigin {
    pub base: String,
    pub filter_expr: Option<Value>,
    pub min_zoom_field: Option<String>,
    pub min_zoom: Option<u8>,
    pub max_zoom: Option<u8>,
}

/// The [`FeatureOrigin`] of node `id`, when it is a `features` node.
///
/// Only a `features` node itself answers. Any other op between it and the
/// asker changes the geometry, and the neighbour tiles' copies of the layer
/// are read as they are bound, so they would not match what the asker
/// receives from its own tile. A `features` node that would fail to build
/// answers nothing either: its own build reports why.
pub(super) fn feature_origin(ctx: &FactoryCtx<'_>, id: &str) -> Option<FeatureOrigin> {
    let spec = ctx.nodes.get(id)?;
    if spec.op != "features" {
        return None;
    }
    let fields = &spec.fields;
    let layer = fields.get("layer")?.as_str()?;
    let is_feature_source = |decl: &spec::SourceDecl| {
        matches!(
            decl,
            spec::SourceDecl::Mvt(_) | spec::SourceDecl::Pmtiles(_) | spec::SourceDecl::GeoJson(_)
        )
    };
    // The same answer `resolve_source` gives, without repeating the
    // warning the `features` node gives for an unnamed source.
    let source = match fields.get("source") {
        Some(v) => {
            let name = v.as_str()?;
            is_feature_source(ctx.sources.get(name)?).then(|| name.to_string())?
        }
        None => {
            let mut matches = ctx.sources.iter().filter(|(_, d)| is_feature_source(d));
            match (matches.next(), matches.next()) {
                (Some((name, _)), None) => name.clone(),
                _ => return None,
            }
        }
    };
    Some(FeatureOrigin {
        base: format!("{source}.{layer}"),
        filter_expr: fields.get("filter-expr").cloned(),
        min_zoom_field: read_optional_string(fields, "min-zoom-field").ok()?,
        min_zoom: read_optional_zoom(fields, "min-zoom").ok()?,
        max_zoom: read_optional_zoom(fields, "max-zoom").ok()?,
    })
}

// ---------------------------------------------------------------------------
// Neighbour tiles

/// Load the 8 neighbour tiles' copies of a feature layer, filtered exactly
/// like the centre tile's: each bound `<base>@dx,dy` binding (see
/// [`ezu_graph::neighbor_binding`]) is run through [`collect_groups`] with
/// the same `filter_expr` / `min_zoom_field` at zoom `z`, and paired with
/// its `(dx, dy)` offset in tiles. Rows run `dy = -1..=1`, then
/// `dx = -1..=1`, skipping the centre.
///
/// A neighbour that is not bound, or whose payload is not a feature layer,
/// is left out. So is one whose extent differs from the frame's: callers
/// place a neighbour's features at `(dx, dy) * extent` from the centre's
/// origin, and that shared world frame only holds when every tile counts
/// the same units across.
///
/// `extent` is the centre layer's, or `0` when the centre tile has no copy
/// of the layer — an MVT encoder leaves out a layer with no features in
/// that tile, yet a neighbour's label or line can still reach across. The
/// first bound neighbour then sets the frame. The extent the groups are in
/// is returned alongside them, so a caller with no layer of its own knows
/// which units to place them in.
pub(super) fn neighbor_feature_groups(
    ctx: &EvalCtx<'_>,
    base: &str,
    extent: u32,
    filter_expr: Option<&maplibre_expr::Expr>,
    min_zoom_field: &Option<String>,
    z: u8,
) -> (i64, Vec<NeighborGroups>) {
    let mut frame = (extent > 0).then_some(i64::from(extent));
    let mut out = Vec::new();
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
            let e = shared.layer.extent.max(1) as i64;
            if *frame.get_or_insert(e) != e {
                continue;
            }
            out.push(NeighborGroups {
                groups: collect_groups(&shared, filter_expr, min_zoom_field, z),
                dx: dx as i64,
                dy: dy as i64,
                overzoom: shared.overzoom,
            });
        }
    }
    (frame.unwrap_or(1), out)
}

/// One neighbour tile's filtered feature groups, from
/// [`neighbor_feature_groups`]: in the neighbour's own `[0, extent]` frame,
/// `(dx, dy)` tiles away from the centre.
pub(super) struct NeighborGroups {
    pub groups: Vec<FeatureGroup>,
    pub dx: i64,
    pub dy: i64,
    /// The neighbour layer's [`SharedLayer::overzoom`].
    pub overzoom: u8,
}

/// The tile `(dx, dy)` away from `tile`, wrapping east–west around the
/// world as the map does; `None` past a pole, where there is no tile.
pub(super) fn neighbour_tile(
    tile: ezu_graph::TileId,
    dx: i64,
    dy: i64,
) -> Option<ezu_graph::TileId> {
    let n = 1i64 << tile.z.min(31);
    let y = i64::from(tile.y) + dy;
    if !(0..n).contains(&y) {
        return None;
    }
    Some(ezu_graph::TileId {
        z: tile.z,
        x: (i64::from(tile.x) + dx).rem_euclid(n) as u32,
        y: y as u32,
    })
}

/// Run a feature-by-feature op over `feats`, and over each of its
/// neighbours when the op was asked for them (see
/// [`ezu_graph::Node::forwards_neighbours`]).
///
/// `op` receives the context to work in, the extent of the frame its
/// groups are in, and the groups, and returns what it makes of them. For a
/// neighbour the context's `tile` is the neighbour's, so an op anchored to
/// the world grid (a dash phase, a hatch, a scatter) lays the neighbour out
/// exactly as the neighbour's own render does. The result carries no
/// overzoom, as a geometry op's output never does.
pub(super) fn map_with_neighbours(
    ctx: &EvalCtx<'_>,
    feats: &FilteredFeatures,
    mut op: impl FnMut(&EvalCtx<'_>, u32, &[FeatureGroup]) -> Result<Vec<FeatureGroup>, EvalError>,
) -> Result<PortValue, EvalError> {
    let groups = op(ctx, feats.extent, &feats.groups)?;
    let mut out = FilteredFeatures::new(feats.extent, groups);
    if ctx.neighbours {
        for n in &feats.neighbours {
            let Some(tile) = neighbour_tile(ctx.tile, n.dx.into(), n.dy.into()) else {
                continue;
            };
            let nctx = EvalCtx {
                tile,
                neighbours: false,
                ..*ctx
            };
            let groups = op(&nctx, n.extent, &n.groups)?;
            out.neighbours
                .push(NeighbourLayer::new(n.dx, n.dy, n.extent, groups));
        }
    }
    Ok(features_port(out))
}

// ---------------------------------------------------------------------------
// Color conversions

/// Straight (non-premultiplied) `[0, 1]` components → `[0, 255]`.
pub(super) fn color_f32_to_u8(c: [f32; 4]) -> [u8; 4] {
    [
        (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[3].clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

pub(super) fn srgb_to_linear_rgba(c: [f32; 4]) -> [f32; 4] {
    fn ch(v: f32) -> f32 {
        if v <= 0.04045 {
            v / 12.92
        } else {
            libm::powf((v + 0.055) / 1.055, 2.4)
        }
    }
    [ch(c[0]), ch(c[1]), ch(c[2]), c[3]]
}

pub(super) fn color_to_premul_u8(c: [f32; 4]) -> [u8; 4] {
    let a = c[3].clamp(0.0, 1.0);
    [
        (c[0] * a * 255.0).round() as u8,
        (c[1] * a * 255.0).round() as u8,
        (c[2] * a * 255.0).round() as u8,
        (a * 255.0).round() as u8,
    ]
}

pub(super) fn tint_alpha_color(c: [u8; 4], alpha_mul: f32) -> tiny_skia::Color {
    let a = ((c[3] as f32) * alpha_mul.clamp(0.0, 1.0)).round() as u8;
    tiny_skia::Color::from_rgba8(c[0], c[1], c[2], a)
}

// ---------------------------------------------------------------------------
// Canvas / raster bridging

/// Build a fresh padded canvas matching the eval ctx.
pub(super) fn make_canvas(ctx: &EvalCtx<'_>) -> Result<Canvas, EvalError> {
    Canvas::new_padded(ctx.canvas.tile_w, ctx.canvas.tile_h, ctx.canvas.pad).ok_or_else(|| {
        EvalError::Other(format!(
            "canvas allocation failed for tile={}x{} pad={}",
            ctx.canvas.tile_w, ctx.canvas.tile_h, ctx.canvas.pad
        ))
    })
}

/// Consume a freshly-painted [`Canvas`] into a zero-copy [`RasterBuf`].
///
/// Uses `Pixmap::take` so the inner pixel `Vec<u8>` flows straight into
/// the graph layer without `to_vec`. Saves a ~1.3 MB memcpy per paint
/// node on a 564×564 padded canvas.
pub(super) fn canvas_into_raster(canvas: Canvas) -> RasterBuf {
    let pixmap = canvas.into_pixmap();
    let (w, h) = (pixmap.width(), pixmap.height());
    RasterBuf {
        width: w,
        height: h,
        pixels: pixmap.take(),
    }
}

/// Padded transparent raster, used when a paint node has no features
/// to draw (still returns a sized buffer so downstream blends work).
pub(super) fn empty_raster(ctx: &EvalCtx<'_>) -> PortValue {
    let (pw, ph) = ctx.canvas.padded_dims();
    PortValue::Raster(Arc::new(RasterBuf::new(pw, ph)))
}

/// An image wrapped in a one-pixel transparent frame, for drawing under an
/// arbitrary transform.
///
/// tiny-skia's `draw_pixmap` fills the transformed image rect and samples it
/// with `SpreadMode::Pad`, so any destination pixel whose centre falls just
/// outside the image repeats the image's edge row or column. That happens
/// routinely: the rect's edges are rounded to whole pixels, and an
/// odd-sized icon centred on a whole-pixel anchor puts both edges on a half
/// pixel, which rounds the covered span one pixel past the image on one side
/// — a ghost copy of the icon's last column (or row) beside it. Bilinear
/// filtering reads half a pixel past every edge as well. The frame makes
/// every such sample transparent, which is what MapLibre gets by padding
/// its atlas entries.
pub(super) struct FramedImage(tiny_skia::Pixmap);

impl FramedImage {
    /// `None` for an empty image.
    pub(super) fn new(img: &RasterBuf) -> Option<FramedImage> {
        if img.width == 0 || img.height == 0 {
            return None;
        }
        let mut framed = tiny_skia::Pixmap::new(img.width + 2, img.height + 2)?;
        let fw = framed.width() as usize;
        let row = img.width as usize * 4;
        let dst = framed.data_mut();
        for (y, src) in img.pixels.chunks_exact(row).enumerate() {
            let at = ((y + 1) * fw + 1) * 4;
            dst[at..at + row].copy_from_slice(src);
        }
        Some(FramedImage(framed))
    }

    /// The image's own size, without the frame.
    pub(super) fn size(&self) -> (f32, f32) {
        ((self.0.width() - 2) as f32, (self.0.height() - 2) as f32)
    }

    /// Draw the image with `t` mapping its own (unframed) pixel space onto
    /// `pm`, exactly as `draw_pixmap` would be called with the bare image.
    pub(super) fn draw(
        &self,
        pm: &mut tiny_skia::PixmapMut<'_>,
        paint: &tiny_skia::PixmapPaint,
        t: tiny_skia::Transform,
    ) {
        pm.draw_pixmap(
            0,
            0,
            self.0.as_ref(),
            paint,
            t.pre_translate(-1.0, -1.0),
            None,
        );
    }
}

pub(super) fn core_tile(ctx: &EvalCtx<'_>) -> CoreTileId {
    CoreTileId::new(ctx.tile.z, ctx.tile.x, ctx.tile.y)
}

// ---------------------------------------------------------------------------
// PortValue downcasting

pub(super) fn features_value(extent: u32, groups: Vec<FeatureGroup>) -> PortValue {
    features_payload(extent, 0, groups)
}

fn features_payload(extent: u32, overzoom: u8, groups: Vec<FeatureGroup>) -> PortValue {
    features_port(FilteredFeatures::new(extent, groups).with_overzoom(overzoom))
}

/// Wrap a payload as a `Features` port value.
pub(super) fn features_port(payload: FilteredFeatures) -> PortValue {
    PortValue::Features(Arc::new(payload) as Arc<dyn Any + Send + Sync>)
}

/// Like [`features_value`], but first drops the geometry that cannot
/// affect the rendered tile.
///
/// A source tile is not clipped to the tile that asks for it. Its own
/// buffer already reaches past the edge, and under overzoom it is an
/// ancestor's geometry scaled up by the zoom difference — eight levels
/// deep, a road that measured ten pixels in its own tile arrives two and
/// a half thousand pixels long, and nearly every feature of the ancestor
/// lands somewhere off this tile entirely. Carried further down the
/// graph, each one is resampled at a pixel-scale step and then stroked
/// dab by dab, so the cost of a tile grows with its ancestor's extent
/// rather than its own.
///
/// The graph knows how far out geometry can still matter — every op
/// between here and the canvas declares its reach — so ask, and keep
/// what is within it. Anything further cannot mark the tile whatever
/// happens downstream.
///
/// `overzoom` is the source layer's (see [`FilteredFeatures::overzoom`]).
pub(super) fn features_value_culled(
    ctx: &EvalCtx<'_>,
    extent: u32,
    overzoom: u8,
    groups: Vec<FeatureGroup>,
) -> PortValue {
    features_port(culled_features(ctx, extent, overzoom, groups))
}

/// The payload [`features_value_culled`] wraps, for a caller with more to
/// add to it.
pub(super) fn culled_features(
    ctx: &EvalCtx<'_>,
    extent: u32,
    overzoom: u8,
    groups: Vec<FeatureGroup>,
) -> FilteredFeatures {
    FilteredFeatures::new(extent, cull_groups(ctx, extent, groups)).with_overzoom(overzoom)
}

/// The culling step of [`features_value_culled`] on its own, for a caller
/// that wants the cheap drop before some costlier work of its own.
pub(super) fn cull_groups(
    ctx: &EvalCtx<'_>,
    extent: u32,
    groups: Vec<FeatureGroup>,
) -> Vec<FeatureGroup> {
    let Some(rect) = ctx.cull_rect() else {
        return groups;
    };
    if extent == 0 {
        return groups;
    }
    // MVT tile-local units to padded-canvas pixels, as the paint ops
    // resolve them.
    let sx = ctx.canvas.tile_w as f64 / extent as f64;
    let sy = ctx.canvas.tile_h as f64 / extent as f64;
    let pad = ctx.canvas.pad as f64;
    let keep = |pts: &[(i32, i32)]| -> bool { ring_touches(pts, sx, sy, pad, rect) };

    groups
        .into_iter()
        .filter_map(|g| {
            let polygons: Vec<_> = g
                .polygons
                .into_iter()
                // A hole only ever removes fill from its own exterior,
                // so the exterior alone decides.
                .filter(|p| keep(&p.exterior))
                .collect();
            let lines: Vec<_> = g.lines.into_iter().filter(|l| keep(l)).collect();
            let points: Vec<_> = g.points.into_iter().filter(|&p| keep(&[p])).collect();
            if polygons.is_empty() && lines.is_empty() && points.is_empty() {
                return None;
            }
            Some(FeatureGroup {
                properties: g.properties,
                polygons,
                lines,
                points,
            })
        })
        .collect()
}

/// Whether a ring's bounding box, in padded-canvas pixels, meets `rect`.
fn ring_touches(
    pts: &[(i32, i32)],
    sx: f64,
    sy: f64,
    pad: f64,
    rect: (f64, f64, f64, f64),
) -> bool {
    let (rx0, ry0, rx1, ry1) = rect;
    let (mut x0, mut x1) = (f64::MAX, f64::MIN);
    let (mut y0, mut y1) = (f64::MAX, f64::MIN);
    for &(x, y) in pts {
        let px = x as f64 * sx + pad;
        let py = y as f64 * sy + pad;
        x0 = x0.min(px);
        x1 = x1.max(px);
        y0 = y0.min(py);
        y1 = y1.max(py);
    }
    // An empty ring has no bounding box and nothing to draw.
    x0 <= x1 && x1 >= rx0 && x0 <= rx1 && y1 >= ry0 && y0 <= ry1
}

/// Accepts list for ports that take either a canvas `Raster` or a
/// native-sized `Sprite`. Filter ops that don't care about the
/// canvas-alignment of their input (e.g. `blur`, `hsl`) use this.
pub(super) const ACCEPTS_RASTER_OR_SPRITE: &[PortKind] = &[PortKind::Raster, PortKind::Sprite];

/// Extract an `Arc<RasterBuf>` from a `PortValue` that is either
/// `Raster` or `Sprite`, returning which variant it came from so the
/// caller can re-wrap the result.
pub(super) fn unwrap_raster_or_sprite(
    v: &PortValue,
    port_name: &str,
) -> Result<(Arc<RasterBuf>, RasterKind), EvalError> {
    match v {
        PortValue::Raster(r) => Ok((r.clone(), RasterKind::Raster)),
        PortValue::Sprite(s) => Ok((s.clone(), RasterKind::Sprite)),
        _ => Err(EvalError::MissingInput(port_name.into())),
    }
}

/// Re-wrap a `RasterBuf` into the same `PortValue` variant it came
/// from — preserves the canvas-vs-native distinction through a
/// pass-through filter node.
pub(super) fn wrap_raster_like(buf: Arc<RasterBuf>, kind: RasterKind) -> PortValue {
    match kind {
        RasterKind::Raster => PortValue::Raster(buf),
        RasterKind::Sprite => PortValue::Sprite(buf),
    }
}

/// Which raster-flavoured [`PortValue`] variant a value originated
/// from. Carried through pass-through filter ops so the output port
/// kind mirrors the input.
#[derive(Debug, Clone, Copy)]
pub(super) enum RasterKind {
    Raster,
    Sprite,
}

/// Reads for a small kernel whose neighbours sit `step` pixels apart, so a
/// filter written for 1-pixel neighbours covers the same ground on a
/// denser canvas. Reads past the edge repeat the edge pixel. A whole-number
/// step reads single pixels; a fractional one, at a pixel ratio such as
/// 1.5, blends the pixels around each neighbour bilinearly.
pub(super) struct NeighbourTaps<'a> {
    src: &'a RasterBuf,
    step: f32,
    /// `step` as whole pixels, when it is one.
    whole: Option<i32>,
}

impl<'a> NeighbourTaps<'a> {
    pub(super) fn new(src: &'a RasterBuf, step: f32) -> Self {
        let whole = (step.fract() == 0.0).then_some(step as i32);
        Self { src, step, whole }
    }

    /// Channel `c` of the neighbour `(kx, ky)` steps from pixel `(x, y)`,
    /// as a float holding the whole channel value for a whole step.
    #[inline]
    pub(super) fn at(&self, x: i32, y: i32, kx: i32, ky: i32, c: usize) -> f32 {
        let (w, h) = (self.src.width, self.src.height);
        let read = |px: usize, py: usize| self.src.pixels[(py * w as usize + px) * 4 + c] as f32;
        if let Some(d) = self.whole {
            let px = (x + kx * d).clamp(0, w as i32 - 1) as usize;
            let py = (y + ky * d).clamp(0, h as i32 - 1) as usize;
            return read(px, py);
        }
        let b = crate::imaging::Bilinear::new(
            w as usize,
            h as usize,
            (x as i64, y as i64),
            (kx as f32 * self.step, ky as f32 * self.step),
        );
        b.lerp(
            read(b.x0, b.y0),
            read(b.x1, b.y0),
            read(b.x0, b.y1),
            read(b.x1, b.y1),
        )
    }
}

/// Resolve a polymorphic Raster/Sprite output kind from the first
/// connected input. Used by `Node::output` for pass-through filter
/// nodes (`blur`, `hsl`, …). Falls back to `Raster` if the input is
/// unconnected (which is a build error anyway for required ports).
pub(super) fn raster_or_sprite_output(input_kinds: &[Option<PortKind>]) -> PortKind {
    match input_kinds.first().and_then(|k| *k) {
        Some(PortKind::Sprite) => PortKind::Sprite,
        _ => PortKind::Raster,
    }
}

/// Accepts list for filter ops that also take a `ScalarField` (`blur`,
/// `warp`, …), giving back the same kind they were given.
pub(super) const ACCEPTS_IMAGE_OR_FIELD: &[PortKind] =
    &[PortKind::Raster, PortKind::Sprite, PortKind::ScalarField];

/// [`raster_or_sprite_output`], extended to a `ScalarField` input for the
/// ops that accept [`ACCEPTS_IMAGE_OR_FIELD`].
pub(super) fn image_or_field_output(input_kinds: &[Option<PortKind>]) -> PortKind {
    match input_kinds.first().and_then(|k| *k) {
        Some(PortKind::ScalarField) => PortKind::ScalarField,
        _ => raster_or_sprite_output(input_kinds),
    }
}

/// The value a field op writes where it has no valid sample to give:
/// the field's `nodata`, or NaN when it has none.
pub(super) fn field_fill(field: &ScalarField) -> f32 {
    field.nodata.unwrap_or(f32::NAN)
}

/// Resample a field at a per-pixel offset, for `warp` and `displace`:
/// output pixel `(x, y)` reads the field at `(x, y) + offset(x, y)`,
/// bilinearly, clamped at the field's edge. Missing samples (`nodata`
/// or NaN) are left out of the blend rather than averaged in, and a
/// read with nothing valid around it writes [`field_fill`]. The result
/// keeps the field's size, `nodata` and `geo_scale`.
pub(super) fn resample_field(
    field: &ScalarField,
    offset: impl Fn(u32, u32) -> (f64, f64),
) -> PortValue {
    let (w, h) = (field.width, field.height);
    if w == 0 || h == 0 {
        return PortValue::ScalarField(Arc::new(field.clone()));
    }
    let fill = field_fill(field);
    let mut values = Vec::with_capacity(field.values.len());
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = offset(x, y);
            let v = crate::imaging::sample_field(
                &field.values,
                w as usize,
                h as usize,
                (i64::from(x), i64::from(y)),
                (dx as f32, dy as f32),
                field.nodata,
            );
            values.push(v.unwrap_or(fill));
        }
    }
    PortValue::ScalarField(Arc::new(ScalarField {
        values: values.into(),
        ..*field
    }))
}

pub(super) fn downcast_features(v: &PortValue) -> Result<Arc<FilteredFeatures>, EvalError> {
    let PortValue::Features(o) = v else {
        return Err(EvalError::Other("expected Features".into()));
    };
    o.clone()
        .downcast::<FilteredFeatures>()
        .map_err(|_| EvalError::Other("features payload type mismatch".into()))
}

pub(super) fn downcast_brush(v: &PortValue) -> Result<Arc<BrushPayload>, EvalError> {
    let PortValue::Brush(o) = v else {
        return Err(EvalError::Other("expected Brush".into()));
    };
    o.clone()
        .downcast::<BrushPayload>()
        .map_err(|_| EvalError::Other("brush payload type mismatch".into()))
}

#[cfg(test)]
mod cull_tests {
    use super::*;
    use ezu_graph::{CanvasInfo, NoAssets, ParamValues, TileId};

    const EXTENT: u32 = 4096;

    /// One line, at a tile-local position given in MVT units.
    fn line_at(x: i32, y: i32) -> FeatureGroup {
        FeatureGroup::synthetic(vec![], vec![vec![(x, y), (x + 10, y + 10)]], vec![])
    }

    /// Cull `groups` at a given reach and report how many groups, and
    /// how much geometry, survived.
    fn cull(influence_pad: u32, groups: Vec<FeatureGroup>) -> Vec<FeatureGroup> {
        let params = ParamValues::new();
        let assets = NoAssets;
        let ctx = EvalCtx {
            tile: TileId {
                z: 13,
                x: 7276,
                y: 3225,
            },
            canvas: CanvasInfo::square(256, 12),
            assets: &assets,
            params: &params,
            rng_seed: 0,
            influence_pad,
            neighbours: false,
        };
        let PortValue::Features(p) = features_value_culled(&ctx, EXTENT, 0, groups) else {
            panic!("not a features port")
        };
        let f = p
            .downcast_ref::<FilteredFeatures>()
            .expect("features payload");
        f.groups
            .iter()
            .map(|g| FeatureGroup::synthetic(g.polygons.clone(), g.lines.clone(), g.points.clone()))
            .collect()
    }

    #[test]
    fn geometry_beyond_the_reach_is_dropped() {
        // 4096 MVT units span 256 px, so 40000 units is far outside.
        let kept = cull(8, vec![line_at(2000, 2000), line_at(40_000, 40_000)]);
        assert_eq!(kept.len(), 1);
    }

    /// Geometry outside the canvas still marks it when something
    /// downstream reaches that far, so the reach decides rather than
    /// the canvas edge.
    #[test]
    fn geometry_within_the_reach_is_kept() {
        // 1 px is 16 MVT units here and the canvas carries 12 px of
        // pad, so -400 units sits 13 px above the padded canvas.
        assert_eq!(cull(0, vec![line_at(1000, -400)]).len(), 0);
        assert_eq!(cull(64, vec![line_at(1000, -400)]).len(), 1);
    }

    /// When nothing downstream can bound its reach, everything stays.
    #[test]
    fn an_unbounded_reach_keeps_everything() {
        let kept = cull(
            u32::MAX,
            vec![line_at(2000, 2000), line_at(400_000, 400_000)],
        );
        assert_eq!(kept.len(), 2);
    }

    /// A group is only dropped once *none* of its geometry survives.
    #[test]
    fn a_group_survives_on_any_one_of_its_shapes() {
        let mixed = FeatureGroup::synthetic(
            vec![],
            vec![vec![(40_000, 40_000), (40_010, 40_010)]],
            vec![(2000, 2000)],
        );
        let kept = cull(8, vec![mixed]);
        assert_eq!(kept.len(), 1);
        assert!(kept[0].lines.is_empty(), "the far line went");
        assert_eq!(kept[0].points.len(), 1, "the near point stayed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_modes_map_indices_past_the_edge() {
        let row = |mode| (-4..8).map(|i| wrap_index(i, 4, mode)).collect::<Vec<_>>();
        let s = Some;
        assert_eq!(
            row(BoundaryMode::Clamp),
            [0, 0, 0, 0, 0, 1, 2, 3, 3, 3, 3, 3].map(s)
        );
        assert_eq!(
            row(BoundaryMode::Mirror),
            [2, 3, 2, 1, 0, 1, 2, 3, 2, 1, 0, 1].map(s)
        );
        assert_eq!(
            row(BoundaryMode::Wrap),
            [0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3].map(s)
        );
        let transparent = row(BoundaryMode::Transparent);
        assert_eq!(&transparent[4..8], &[0, 1, 2, 3].map(s));
        assert!(transparent[..4]
            .iter()
            .chain(&transparent[8..])
            .all(Option::is_none));
    }
}
