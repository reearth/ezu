//! Shared label placement: `label-placement` decides every label layer of
//! a recipe at once, `text-draw` rasterizes one layer's winners.
//!
//! MapLibre runs **one** collision index for all symbol layers, so a POI
//! label can knock out an overlapping road name. ezu splits that into
//! three stages, wired explicitly in the recipe:
//!
//! 1. `text-labels` (one per label layer) shapes its layer's labels into
//!    placement candidates — world-space, deterministic, neighbour tiles
//!    included — and emits them on a `Labels` port.
//! 2. `label-placement` fans those in (`labels[i]`, bottom layer first,
//!    like `stack`) and runs one deterministic greedy placement over the
//!    lot. Priority is top-down: the **last** entry places first, matching
//!    maplibre-gl-js, which walks symbol layers from the top of the style.
//! 3. `text-draw` takes a layer's candidates plus those decisions and
//!    draws only the labels that placed.
//!
//! Per-layer styling and paint order are untouched — only the placement
//! decision is global. A recipe that needs no sharing keeps using the
//! self-contained `text` node, which places its own candidates through the
//! same engine.

use std::collections::HashMap;
use std::sync::Arc;

use ezu_graph::{
    schema_frag, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError,
    Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{canvas_into_raster, empty_raster, make_canvas};
use ezu_core::text::{
    collide, draw, draw_line, FaceEntry, Font, GlyphPlacement, LabelCandidate, OutlineSdfCache,
    SectionPaint, StackEntry, TextBlock, TextPaint,
};

/// Accepts-list for a `Labels` port.
pub(super) const ACCEPTS_LABELS: &[PortKind] = &[PortKind::Labels];

/// One label layer's placement candidates plus everything needed to draw
/// them, flowing along a `Labels` port. `draws` is index-aligned with
/// `candidates`, so a [`collide::Placement`] selects both.
pub(super) struct LabelSet {
    /// Content identity of this set, matched against a
    /// [`PlacedLabels`] entry by the drawing stage. Derived from the
    /// producing node's parameters and its candidates, never from an
    /// allocation address, so it survives a cache hit on either side.
    pub id: u64,
    pub candidates: Vec<LabelCandidate>,
    pub draws: Vec<LabelDraw>,
    /// MapLibre `text-padding`, for the off-canvas reject at draw time.
    pub padding_px: f32,
    /// Whether the candidates were placed at all: with collision off every
    /// label draws, and the off-canvas reject is skipped (matching the
    /// pre-collision behaviour).
    pub collide: bool,
    /// Render outline glyphs through the SDF path.
    pub outline_sdf: bool,
}

/// One label's draw payload. `blocks` / `placements` are in the local
/// world-pixel frame (this tile's origin at 0); the canvas pad is added at
/// draw time. `fonts` is the label's flat font stack (its glyphs' `font`
/// indices point into it), `paints` the per-section fill table; both are
/// shared across a group's labels via `Arc`.
pub(super) enum LabelDraw {
    /// A point label: one laid-out block per anchor variant (a single entry
    /// for a fixed anchor), selected by the winning variant index.
    Point {
        blocks: Vec<Arc<TextBlock>>,
        anchor: (f32, f32),
        paint: TextPaint,
        fonts: Arc<Vec<StackEntry>>,
        paints: Arc<Vec<SectionPaint>>,
    },
    /// A line label: one block walked along the path, with a per-glyph
    /// placement and the perpendicular `offset-em` shift applied at draw.
    Line {
        block: Arc<TextBlock>,
        placements: Vec<GlyphPlacement>,
        perp_px: f32,
        paint: TextPaint,
        fonts: Arc<Vec<StackEntry>>,
        paints: Arc<Vec<SectionPaint>>,
    },
}

impl LabelSet {
    /// The flat font stack of every label in the set, for [`FaceCache`].
    pub(super) fn stacks(&self) -> impl Iterator<Item = &[StackEntry]> {
        self.draws.iter().map(|d| match d {
            LabelDraw::Point { fonts, .. } | LabelDraw::Line { fonts, .. } => fonts.as_slice(),
        })
    }
}

/// The set id for one label layer: the producing node's parameter hash
/// folded with every candidate's identity. Two label layers of one recipe
/// collide here only when their parameters *and* their candidates match,
/// in which case they place identically anyway.
pub(super) fn set_id(param_hash: u64, candidates: &[LabelCandidate]) -> u64 {
    let mut h = Xxh3::new();
    h.update(&param_hash.to_le_bytes());
    for c in candidates {
        h.update(&c.world_ax.to_le_bytes());
        h.update(&c.world_ay.to_le_bytes());
        h.update(c.text.as_bytes());
        h.update(&[0]);
        h.update(&c.style_id.to_le_bytes());
        h.update(&c.sort_key.to_bits().to_le_bytes());
    }
    h.digest()
}

/// What a `label-placement` node decided, keyed by each input layer's
/// [`LabelSet::id`] so a drawing node finds its own decisions without the
/// recipe repeating a layer index.
pub(super) struct PlacedLabels {
    by_set: HashMap<u64, Vec<collide::Placement>>,
}

impl PlacedLabels {
    /// The placements for one label set, or `None` when that set never
    /// reached this placement node (a mis-wired recipe).
    fn get(&self, id: u64) -> Option<&[collide::Placement]> {
        self.by_set.get(&id).map(Vec::as_slice)
    }
}

/// One `rustybuzz::Face` per distinct outline font, built once per eval.
/// Shaping, coverage, and outline extraction all take a face, so building
/// each once here (rather than reparsing the ~20 MB font file per glyph as
/// coverage itemization would) is the dominant win for a text node.
///
/// Keyed by the font's `Arc` identity: every assembled flat stack is cloned
/// from the node's registry stacks, so its outline entries share these fonts
/// and hit the cache. Handing out cheap [`rustybuzz::Face`] clones lets each
/// label build a [`FaceEntry`] view without reparsing.
pub(super) struct FaceCache<'a> {
    faces: HashMap<*const Font, rustybuzz::Face<'a>>,
}

impl<'a> FaceCache<'a> {
    /// Build a face for every distinct outline font across `stacks`.
    pub(super) fn from_stacks(stacks: impl Iterator<Item = &'a [StackEntry]>) -> FaceCache<'a> {
        let mut faces: HashMap<*const Font, rustybuzz::Face<'a>> = HashMap::new();
        for stack in stacks {
            for entry in stack {
                if let StackEntry::Outline(font) = entry {
                    faces
                        .entry(Arc::as_ptr(font))
                        .or_insert_with(|| font.face());
                }
            }
        }
        FaceCache { faces }
    }

    /// A prepared view of `stack`, aligned index-for-index, whose outline
    /// entries carry a cheap clone of the pre-built face.
    pub(super) fn view<'b>(&'b self, stack: &'b [StackEntry]) -> Vec<FaceEntry<'b>> {
        stack
            .iter()
            .map(|entry| match entry {
                StackEntry::Outline(font) => FaceEntry::Outline {
                    font,
                    face: self
                        .faces
                        .get(&Arc::as_ptr(font))
                        .cloned()
                        .unwrap_or_else(|| font.face()),
                },
                StackEntry::Sdf(s) => FaceEntry::Sdf(s),
            })
            .collect()
    }
}

/// Rasterize the labels of `set` that `placed` chose, in placement order.
/// `faces` supplies the pre-built faces for the set's font stacks.
///
/// Placement runs over the 3×3 tile window, so a winner may sit entirely
/// outside this canvas (it belongs to a neighbour tile and only mattered
/// because it blocked something); such labels are rejected here rather than
/// clipped, which keeps the work off the hot path and the pixels identical
/// to what the neighbour draws.
pub(super) fn draw_labels(
    ctx: &EvalCtx<'_>,
    set: &LabelSet,
    placed: &[collide::Placement],
    faces: &FaceCache<'_>,
) -> Result<PortValue, EvalError> {
    if placed.is_empty() {
        return Ok(empty_raster(ctx));
    }
    let mut canvas = make_canvas(ctx)?;
    let pad = canvas.pad() as f32;
    let padded_w = canvas.tile_width() as f32 + 2.0 * pad;
    let padded_h = canvas.tile_height() as f32 + 2.0 * pad;
    // Per-eval SDF glyph cache for outline fonts (`None` keeps the vector
    // path). Outline glyphs are rasterized to an SDF once and reused.
    let sdf_cache = set.outline_sdf.then(OutlineSdfCache::new);
    let pm = canvas.pixmap_mut();
    let mut pm = pm.as_mut();
    for p in placed {
        let Some(d) = set.draws.get(p.cand) else {
            continue;
        };
        match d {
            LabelDraw::Point {
                blocks,
                anchor,
                paint,
                fonts,
                paints,
            } => {
                let block = blocks.get(p.variant).unwrap_or(&blocks[0]);
                let (ax, ay) = (anchor.0 + pad, anchor.1 + pad);
                if set.collide {
                    let bb = block.bbox;
                    let s = paint.size_px;
                    let min_x = ax + bb.min_x * s - set.padding_px;
                    let max_x = ax + bb.max_x * s + set.padding_px;
                    let min_y = ay + bb.min_y * s - set.padding_px;
                    let max_y = ay + bb.max_y * s + set.padding_px;
                    if max_x < 0.0 || min_x > padded_w || max_y < 0.0 || min_y > padded_h {
                        continue;
                    }
                }
                let view = faces.view(fonts);
                draw(
                    block,
                    &view,
                    &mut pm,
                    (ax, ay),
                    paint,
                    paints,
                    sdf_cache.as_ref(),
                );
            }
            LabelDraw::Line {
                block,
                placements,
                perp_px,
                paint,
                fonts,
                paints,
            } => {
                // The margin covers one glyph's reach from its centre sample.
                let margin = paint.size_px + set.padding_px + perp_px.abs();
                let (mut min_x, mut min_y, mut max_x, mut max_y) =
                    (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for g in placements {
                    min_x = min_x.min(g.x);
                    max_x = max_x.max(g.x);
                    min_y = min_y.min(g.y);
                    max_y = max_y.max(g.y);
                }
                if max_x + pad + margin < 0.0
                    || min_x + pad - margin > padded_w
                    || max_y + pad + margin < 0.0
                    || min_y + pad - margin > padded_h
                {
                    continue;
                }
                let shifted: Vec<GlyphPlacement> = placements
                    .iter()
                    .map(|g| GlyphPlacement {
                        x: g.x + pad,
                        y: g.y + pad,
                        angle: g.angle,
                    })
                    .collect();
                let view = faces.view(fonts);
                draw_line(
                    block,
                    &view,
                    &mut pm,
                    &shifted,
                    *perp_px,
                    paint,
                    paints,
                    sdf_cache.as_ref(),
                );
            }
        }
    }
    log_outline_sdf_stats(sdf_cache.as_ref());
    Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
}

/// Downcast a `Labels` port value to `T`, or report which port failed.
fn labels_input<T: Send + Sync + 'static>(
    value: Option<&PortValue>,
    port: &str,
) -> Result<Arc<T>, EvalError> {
    let value = value.ok_or_else(|| EvalError::MissingInput(port.into()))?;
    let PortValue::Labels(opaque) = value else {
        return Err(EvalError::Other(format!(
            "port `{port}`: expected labels, got {}",
            value.kind()
        )));
    };
    opaque
        .clone()
        .downcast::<T>()
        .map_err(|_| EvalError::Other(format!("port `{port}`: unexpected labels payload")))
}

/// `label-placement` — `Labels… -> Labels`. One deterministic greedy
/// placement over every label layer wired into it.
struct LabelPlacementNode {
    ports: Vec<PortSpec>,
}

impl Node for LabelPlacementNode {
    fn op_name(&self) -> &'static str {
        "label-placement"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Labels
    }
    fn coord_space(&self) -> CoordSpace {
        // Every candidate is world-anchored, so adjacent tiles reach the same
        // decisions and may share this node's cache entry.
        CoordSpace::World
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let mut sets: Vec<Arc<LabelSet>> = Vec::with_capacity(inputs.len());
        for (ix, slot) in inputs.iter().enumerate() {
            sets.push(labels_input::<LabelSet>(
                slot.as_ref(),
                &format!("labels[{ix}]"),
            )?);
        }
        // MapLibre places symbol layers top-down, so the topmost label layer
        // (the last `labels[i]`, drawn over the rest) gets priority. Reversing
        // here keeps the recipe's array in paint order, like `stack`'s.
        let ordered: Vec<&[LabelCandidate]> =
            sets.iter().rev().map(|s| s.candidates.as_slice()).collect();
        let placed = collide::place_layers(&ordered, collide::COLLISION_CELL_PX);

        let mut by_set: HashMap<u64, Vec<collide::Placement>> = HashMap::with_capacity(sets.len());
        let mut total = 0usize;
        for (set, placements) in sets.iter().rev().zip(placed) {
            total += placements.len();
            // Two structurally identical label layers hash to one id; the first
            // keeps the entry (they place the same labels either way).
            by_set.entry(set.id).or_insert(placements);
        }
        tracing::debug!(
            layers = sets.len(),
            candidates = sets.iter().map(|s| s.candidates.len()).sum::<usize>(),
            placed = total,
            "label-placement: shared collision index",
        );
        Ok(PortValue::Labels(Arc::new(PlacedLabels { by_set })))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"label-placement");
        // Layer count disambiguates otherwise identical input chains.
        h.update(&(self.ports.len() as u64).to_le_bytes());
    }
}

/// Interned `&'static str` layer-port names (`labels[0]`, `labels[1]`, …).
/// `PortSpec::name` is `&'static str`; the pool grows once per distinct
/// index ever built and is shared across every instance, so the leak is
/// bounded by the widest placement seen, not by the number of builds.
fn labels_port_name(ix: usize) -> &'static str {
    static POOL: std::sync::OnceLock<std::sync::Mutex<Vec<&'static str>>> =
        std::sync::OnceLock::new();
    let mut pool = POOL
        .get_or_init(|| std::sync::Mutex::new(Vec::new()))
        .lock()
        .expect("label-placement port-name pool poisoned");
    while pool.len() <= ix {
        let name: &'static str = Box::leak(format!("labels[{}]", pool.len()).into_boxed_str());
        pool.push(name);
    }
    pool[ix]
}

pub(super) struct LabelPlacementFactory;
impl NodeFactory for LabelPlacementFactory {
    fn op_name(&self) -> &'static str {
        "label-placement"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let arr = fields
            .get("labels")
            .ok_or_else(|| FactoryError::MissingField("labels".into()))?
            .as_array()
            .ok_or_else(|| FactoryError::BadField {
                field: "labels".into(),
                msg: "expected an array of `@node-ref` strings".into(),
            })?;
        if arr.is_empty() {
            return Err(FactoryError::BadField {
                field: "labels".into(),
                msg: "needs at least one label layer".into(),
            });
        }
        let mut ports = Vec::with_capacity(arr.len());
        let mut connections = Vec::with_capacity(arr.len());
        for (ix, entry) in arr.iter().enumerate() {
            let s = entry.as_str().ok_or_else(|| FactoryError::BadField {
                field: "labels".into(),
                msg: format!("entry {ix}: expected a `@node-ref` string"),
            })?;
            let id = match ezu_style::FieldRef::classify(s) {
                ezu_style::FieldRef::Node(id) => id.to_string(),
                _ => {
                    return Err(FactoryError::BadField {
                        field: "labels".into(),
                        msg: format!("entry {ix}: expected `@node-ref`, got `{s}`"),
                    })
                }
            };
            let name = labels_port_name(ix);
            ports.push(PortSpec::new(name, ACCEPTS_LABELS));
            connections.push(Connection {
                port: name.into(),
                src: id,
            });
        }
        Ok(BuiltNode {
            node: Box::new(LabelPlacementNode { ports }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Place the labels of every `text-labels` layer against one shared collision index, the way maplibre-gl-js does — so a POI label can knock out an overlapping road name. `labels` lists the layers bottom-first (paint order, as in `stack`); priority is top-down, so the last entry places first and wins. Feed the result to each layer's `text-draw` node.",
            "properties": {
                "labels": {
                    "type": "array",
                    "minItems": 1,
                    "items": schema_frag::node_ref(),
                    "description": "`text-labels` layers, bottom first. The topmost layer places first."
                }
            },
            "required": ["labels"],
        })
    }
}

ezu_graph::submit_node!(LabelPlacementFactory);

/// `text-draw` — `Labels + Labels -> Raster`. Draw one label layer's
/// winners, as decided by the shared `label-placement` node.
struct TextDrawNode {
    /// Mirrors the `text` node's `max-extent-px`: the canvas pad this
    /// layer's labels need to survive a tile border un-clipped.
    max_extent_px: f32,
    ports: Vec<PortSpec>,
}

impl Node for TextDrawNode {
    fn op_name(&self) -> &'static str {
        "text-draw"
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
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let set = labels_input::<LabelSet>(inputs[0].as_ref(), "labels")?;
        let placement = labels_input::<PlacedLabels>(inputs[1].as_ref(), "placement")?;
        let placed = placement.get(set.id).ok_or_else(|| {
            EvalError::Other(
                "text-draw: the `labels` layer is absent from the `placement` node — wire the \
                 same `text-labels` node into both"
                    .into(),
            )
        })?;
        let faces = FaceCache::from_stacks(set.stacks());
        draw_labels(ctx, &set, placed, &faces)
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"text-draw");
        h.update(&self.max_extent_px.to_bits().to_le_bytes());
    }
}

pub(super) struct TextDrawFactory;
impl NodeFactory for TextDrawFactory {
    fn op_name(&self) -> &'static str {
        "text-draw"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let mut connections = Vec::with_capacity(2);
        for port in ["labels", "placement"] {
            let value = fields
                .get(port)
                .ok_or_else(|| FactoryError::MissingField(port.into()))?;
            let src = match value.as_str().map(ezu_style::FieldRef::classify) {
                Some(ezu_style::FieldRef::Node(id)) => id.to_string(),
                _ => {
                    return Err(FactoryError::BadField {
                        field: port.into(),
                        msg: "expected a `@node-ref` string".into(),
                    })
                }
            };
            connections.push(Connection {
                port: port.into(),
                src,
            });
        }
        let max_extent_px = fields
            .get("max-extent-px")
            .and_then(Value::as_f64)
            .unwrap_or(super::text::DEFAULT_MAX_EXTENT_PX as f64)
            as f32;
        Ok(BuiltNode {
            node: Box::new(TextDrawNode {
                max_extent_px,
                ports: vec![
                    PortSpec::new("labels", ACCEPTS_LABELS),
                    PortSpec::new("placement", ACCEPTS_LABELS),
                ],
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Draw one label layer's placed labels: `labels` is the layer's `text-labels` node, `placement` the shared `label-placement` node that decided it (wire the same `text-labels` node into both). Styling and layout live on `text-labels`; this node only rasterizes the winners.",
            "properties": {
                "labels": schema_frag::node_ref(),
                "placement": schema_frag::node_ref(),
                "max-extent-px": { "type": "number", "minimum": 0,
                    "description": "Canvas pad (px) this layer's labels need to cross a tile border un-clipped. Mirrors the `text-labels` field of the same name." },
            },
            "required": ["labels", "placement"],
        })
    }
}

ezu_graph::submit_node!(TextDrawFactory);

/// Emit a debug summary of the per-eval outline→SDF cache: unique glyphs
/// rasterized, reuse count, and the bytes their bitmaps hold. Nothing is
/// logged when the SDF path is off.
fn log_outline_sdf_stats(cache: Option<&OutlineSdfCache>) {
    if let Some(cache) = cache {
        let s = cache.stats();
        if s.built > 0 || s.hits > 0 {
            tracing::debug!(
                built = s.built,
                hits = s.hits,
                bitmap_bytes = s.bitmap_bytes,
                "text: outline glyphs rendered through the SDF path"
            );
        }
    }
}
