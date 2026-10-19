//! Deterministic label collision & deduplication (MapLibre's greedy
//! screen-space placement, made tile-independent).
//!
//! MapLibre places labels greedily in screen space, "tiles nearest the
//! viewport centre first", through **one collision index shared by every
//! symbol layer**. A per-tile renderer can't reproduce the
//! viewport-centre part, but everything else is made **deterministic in
//! world space** so neighbouring tiles reach identical decisions and
//! borders stay seamless:
//!
//! 1. Candidates come from the tile's own features **plus the 3×3
//!    neighbour tiles'** features (all evaluated with the same
//!    expressions), so every tile sees the same set for any label that
//!    straddles a border.
//! 2. **Dedup** — the same feature appears in several tiles (MVT buffer +
//!    neighbours). The key is `(layer, text, quantized world anchor)`;
//!    one candidate survives per key. Layers dedup separately: two
//!    layers labelling the same feature are two labels in MapLibre.
//!    This only folds copies of one label together; dropping *different*
//!    labels that say the same thing is the separate, opt-in
//!    [`LabelCandidate::dedupe`] rule.
//! 3. **Total order** — `(layer ↑, sort-key ↑, place rank ↑, quantized
//!    anchor y, quantized anchor x, text)`. `layer` is the cross-layer
//!    priority rank: MapLibre walks the style's symbol layers
//!    **top-down**, so a layer drawn above another places first and can
//!    knock the lower layer's labels out (verified against
//!    maplibre-gl-js). Its own `symbol-sort-key` never crosses a layer
//!    boundary — layer rank dominates. Under both comes the
//!    [`PlaceRank`]: within a layer maplibre-gl-js places a bucket's
//!    symbols in tile feature order, which decides which of two equally
//!    ranked labels wins their overlap. No tile-local quantity enters, so
//!    the order is identical on every tile.
//! 4. **Greedy grid insertion** — each candidate carries one or more
//!    *variants*: alternative box sets tried in order (a
//!    `text-variable-anchor` label offers one single-box variant per
//!    anchor; a line label offers one variant holding a box per glyph).
//!    A variant places only if every box in it is free, and then
//!    reserves them all; the first free variant wins, and a candidate
//!    with no free variant drops. [`Overlap`] relaxes that: `Always`
//!    places the first variant without looking, `Fallback` places it once
//!    every variant has turned out blocked. Either still blocks later
//!    labels, unless `ignore_placement` skips insertion.
//!
//! Point and line labels share this one index, so a POI label and a
//! street name compete exactly as they do in MapLibre.
//!
//! All quantities here are derived from **world-space** inputs — the
//! world anchor (exact integer tile-frame coordinate) and the em box ×
//! size — never from tile-local floats that differ between frames, so
//! two tiles evaluating the shared window agree bit-for-bit.

use std::collections::{HashMap, HashSet};

/// World-anchor dedup/order quantum, in tile-extent units (e.g. 1/4096
/// of a tile at the MVT default extent). The same point feature is
/// re-quantized independently in each tile it appears in (MVT clips and
/// quantizes geometry per tile), so its anchor can differ by a unit or
/// two between a tile's own copy and a neighbour's. Snapping the anchor
/// to this grid before dedup/ordering absorbs that noise while staying
/// far finer than any glyph, so genuinely distinct labels never merge.
pub const DEDUP_QUANTUM: i64 = 4;

/// Default collision grid cell, in canvas pixels. Coarse enough that a
/// label touches only a handful of cells, fine enough that overlap
/// queries stay local.
pub const COLLISION_CELL_PX: f32 = 32.0;

/// Axis-aligned box in the shared world-pixel frame (y down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
}

impl Aabb {
    /// Grow the box by `pad` on every side.
    pub fn inflate(self, pad: f32) -> Aabb {
        Aabb {
            min_x: self.min_x - pad,
            min_y: self.min_y - pad,
            max_x: self.max_x + pad,
            max_y: self.max_y + pad,
        }
    }

    /// Whether two boxes overlap (touching edges do not count).
    pub fn intersects(&self, o: &Aabb) -> bool {
        self.min_x < o.max_x && o.min_x < self.max_x && self.min_y < o.max_y && o.min_y < self.max_y
    }
}

/// A coarse uniform grid over the world-pixel frame for overlap queries.
/// Boxes are bucketed by the cells they touch; a query tests only boxes
/// sharing a cell. Cell coordinates are `floor(px / cell)`, so the grid
/// covers the whole (possibly negative) 3×3 window without an origin.
#[derive(Debug)]
pub struct Grid {
    cell: f32,
    cells: HashMap<(i32, i32), Vec<usize>>,
    boxes: Vec<Aabb>,
}

impl Grid {
    pub fn new(cell_px: f32) -> Grid {
        Grid {
            cell: cell_px.max(1.0),
            cells: HashMap::new(),
            boxes: Vec::new(),
        }
    }

    fn cell_span(&self, b: &Aabb) -> (i32, i32, i32, i32) {
        let lo_x = (b.min_x / self.cell).floor() as i32;
        let hi_x = (b.max_x / self.cell).floor() as i32;
        let lo_y = (b.min_y / self.cell).floor() as i32;
        let hi_y = (b.max_y / self.cell).floor() as i32;
        (lo_x, hi_x, lo_y, hi_y)
    }

    /// Whether `b` overlaps any already-inserted box.
    pub fn intersects_any(&self, b: &Aabb) -> bool {
        let (lo_x, hi_x, lo_y, hi_y) = self.cell_span(b);
        for cy in lo_y..=hi_y {
            for cx in lo_x..=hi_x {
                if let Some(ids) = self.cells.get(&(cx, cy)) {
                    if ids.iter().any(|&i| self.boxes[i].intersects(b)) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Insert `b` so later queries collide against it.
    pub fn insert(&mut self, b: Aabb) {
        let id = self.boxes.len();
        let (lo_x, hi_x, lo_y, hi_y) = self.cell_span(&b);
        self.boxes.push(b);
        for cy in lo_y..=hi_y {
            for cx in lo_x..=hi_x {
                self.cells.entry((cx, cy)).or_default().push(id);
            }
        }
    }
}

/// Where a candidate sits in MapLibre's within-layer placement order.
/// maplibre-gl-js walks a bucket's symbols in **tile feature order** (after
/// the `symbol-sort-key` sort), and a feature's own symbols in the order
/// they were generated — its points, or its anchors along a line. Two
/// labels of equal sort key resolve their overlap by that order alone, so
/// reproducing it is what makes ezu pick the same winners.
///
/// `tile` carries the source tile index, lifting the per-tile order to a
/// total order over the whole 3×3 window: any two candidates rank the same
/// way in every tile that sees them both, so seams stay seamless. Which
/// tile leads is arbitrary (the reference orders tiles by distance to the
/// viewport centre, which a per-tile renderer cannot know).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct PlaceRank {
    /// Source tile index (x, y) at the render zoom.
    pub tile: (i64, i64),
    /// The feature's position in its tile layer, after filtering.
    pub feature: u32,
    /// The symbol's position within its feature.
    pub symbol: u32,
}

/// What a label does when its boxes collide with labels placed before it.
///
/// The discriminants are stable (`Never` 0, `Always` 1, `Fallback` 2), so
/// a caller can hash the mode as a byte and keep the hash the older boolean
/// `allow-overlap` gave.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Overlap {
    /// Place at the first free variant; with none free, drop. MapLibre's
    /// default (`*-allow-overlap: false`, `*-overlap: never`).
    #[default]
    Never = 0,
    /// Place at the first variant without trying the others, whatever it
    /// overlaps (MapLibre `*-allow-overlap: true`, `*-overlap: always`).
    Always = 1,
    /// Try every variant as `Never` does; when none is free, place the
    /// first anyway, overlapping, rather than drop. A label that must show
    /// but would rather move out of the way first.
    Fallback = 2,
}

/// One label placement candidate, in world-space terms only. Point and
/// line labels share this shape: they differ only in how their
/// [`variants`](Self::variants) are built.
#[derive(Debug, Clone)]
pub struct LabelCandidate {
    /// MapLibre `symbol-sort-key`: lower places first, within a layer.
    /// Absent = 0.
    pub sort_key: f64,
    /// Tie-break under `sort_key`: MapLibre's tile feature order.
    pub rank: PlaceRank,
    /// World anchor in tile-extent units (exact integer:
    /// `tile_index × extent + local`), identical across the tiles that
    /// share this feature. For a line label it is the label-centre sample
    /// point.
    pub world_ax: i64,
    pub world_ay: i64,
    /// The evaluated label text, part of the dedup key and the final
    /// order tie-break.
    pub text: String,
    /// Resolved label-style identity (e.g. the font stack's `font_id`).
    /// Two features with the same text in the same quantized cell but
    /// different styles have different collision boxes, so the style joins
    /// the dedup key and the order tie-break — otherwise a tile and its
    /// neighbour, which see those features in different insertion orders,
    /// could pick different survivors and diverge at the seam.
    pub style_id: u64,
    /// Alternative collision-box sets in the shared world-pixel frame,
    /// each already inflated by `padding-px`, tried in declaration order.
    /// A variant places only when *every* box in it is free, and then
    /// reserves all of them; the winning index is reported as
    /// [`Placement::variant`].
    ///
    /// A fixed-anchor point label carries one single-box variant; a
    /// MapLibre `text-variable-anchor` label one per anchor; a line label
    /// one variant holding a box per glyph (MapLibre's along-line
    /// collision circles, mapped onto the AABB grid).
    pub variants: Vec<Vec<Aabb>>,
    /// Label-centre anchor in the shared world-pixel frame — the distance
    /// reference for the same-label repeat check.
    pub anchor_x: f32,
    pub anchor_y: f32,
    /// MapLibre's line-label repeat distance (px, `symbol-spacing / 2`):
    /// a candidate whose anchor is nearer than this to an earlier
    /// same-label anchor is dropped, so a street name doesn't reappear on
    /// every branch of its own road. Zero disables the check (point
    /// labels).
    pub repeat_px: f32,
    /// Same-value dedupe: `(key, distance px)`. A candidate is dropped when
    /// a label of the same layer with an equal key has already been
    /// *placed* nearer than the distance (anchor to anchor, in the shared
    /// world-pixel frame). Labels that are dropped or lose their collision
    /// suppress nothing, so one of a run of equal labels always gets its
    /// chance to show. `None` opts out.
    ///
    /// This is not the dedup of identical copies (see
    /// [`place_layers_detailed`]): those are one label seen from several
    /// tiles, while this drops distinct labels that would repeat the same
    /// information, such as one name on each of several neighbouring
    /// features that share it.
    /// The key is computed from the feature itself, so every tile that sees
    /// a candidate gives it the same key and the decision stays
    /// deterministic across tiles.
    pub dedupe: Option<(String, f32)>,
    /// What to do when every variant (or, for `Always`, the first) collides.
    pub overlap: Overlap,
    /// MapLibre `*-ignore-placement`: don't block later labels (skip
    /// inserting this candidate's boxes into the grid).
    pub ignore_placement: bool,
}

impl LabelCandidate {
    /// The quantized anchor cell used for dedup and ordering (floor
    /// division so negative anchors bucket consistently).
    fn quant(&self) -> (i64, i64) {
        (
            self.world_ax.div_euclid(DEDUP_QUANTUM),
            self.world_ay.div_euclid(DEDUP_QUANTUM),
        )
    }
}

/// Anchors already taken by each `(layer, text, style)` label, in the shared
/// world-pixel frame — the state the line-label repeat filter consults.
type RepeatAnchors<'a> = HashMap<(usize, &'a str, u64), Vec<(f32, f32)>>;

/// Anchors of the labels placed so far under each `(layer, dedupe key)`, in
/// the shared world-pixel frame — the state the same-value dedupe consults.
type DedupeAnchors<'a> = HashMap<(usize, &'a str), Vec<(f32, f32)>>;

/// One placed label: which candidate it came from, and which of its
/// variants was chosen (an index into [`LabelCandidate::variants`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub cand: usize,
    pub variant: usize,
    /// The chosen boxes overlap a label placed earlier: an
    /// [`Overlap::Always`] or [`Overlap::Fallback`] label shown where it
    /// would otherwise have lost.
    pub overlapped: bool,
}

/// Deterministically dedup, order, and greedily place every label layer of
/// a recipe against **one** collision index. `layers` holds each layer's
/// candidates in **priority order** — the first list places first, so a
/// caller passes its label layers top-down (MapLibre's own order). Returns
/// one placement list per layer, index-aligned with `layers`, each in
/// placement order.
///
/// The layer index joins the dedup and repeat keys, so layers never merge
/// each other's labels, and it dominates the total order: a
/// `symbol-sort-key` orders labels only inside its own layer.
///
/// Determinism: the result depends only on world-space quantities that
/// are identical across every tile sharing the 3×3 window — the total
/// order and dedup key use the layer index, quantized world anchor, text,
/// sort key and [`PlaceRank`], and collision boxes are in the shared
/// world-pixel frame.
/// No tile-local input enters. Variants are tried in declaration order
/// (identical on every tile), so the seam stays seamless.
///
/// Between the dedup of identical copies and the collision step comes
/// MapLibre's repeat filter: a candidate within
/// [`LabelCandidate::repeat_px`] of an earlier same-label anchor is
/// dropped. Like the reference it consumes the anchor whether or not the
/// label goes on to win its collision, so a blocked candidate still keeps
/// its neighbours away.
///
/// After it, still before the collision step, comes the same-value dedupe
/// ([`LabelCandidate::dedupe`]). Unlike the repeat filter it counts only
/// labels that were placed, so a dropped or blocked label never hides its
/// duplicates, and a deduped label takes no space.
pub fn place_layers(layers: &[&[LabelCandidate]], cell_px: f32) -> Vec<Vec<Placement>> {
    place_layers_detailed(layers, cell_px)
        .into_iter()
        .map(|l| l.placed)
        .collect()
}

/// One layer's outcome from [`place_layers_detailed`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayerOutcome {
    /// The placed labels, in placement order (what [`place_layers`]
    /// returns for the layer).
    pub placed: Vec<Placement>,
    /// Candidates (indices into the layer's list) that reached the
    /// collision test and found no free variant, in placement order.
    /// Copies folded by the dedup of identical copies and line labels
    /// dropped by the repeat filter are not listed: neither ever competed
    /// for space.
    pub rejected: Vec<usize>,
    /// Candidates dropped by the same-value dedupe
    /// ([`LabelCandidate::dedupe`]) because an equal-keyed label had
    /// already placed nearby, in placement order.
    pub deduped: Vec<usize>,
}

/// [`place_layers`], also reporting the candidates that lost their
/// collision test or were dropped by the same-value dedupe, for callers
/// that show what placement turned down.
pub fn place_layers_detailed(layers: &[&[LabelCandidate]], cell_px: f32) -> Vec<LayerOutcome> {
    // Total order: layer ↑, sort-key ↑, quantized anchor (y, x), text.
    let mut order: Vec<(usize, usize)> = layers
        .iter()
        .enumerate()
        .flat_map(|(li, cands)| (0..cands.len()).map(move |i| (li, i)))
        .collect();
    let at = |(li, i): (usize, usize)| -> &LabelCandidate { &layers[li][i] };
    order.sort_by(|&a, &b| {
        let (ca, cb) = (at(a), at(b));
        a.0.cmp(&b.0)
            .then_with(|| ca.sort_key.total_cmp(&cb.sort_key))
            .then_with(|| ca.rank.cmp(&cb.rank))
            .then_with(|| ca.quant().1.cmp(&cb.quant().1))
            .then_with(|| ca.quant().0.cmp(&cb.quant().0))
            .then_with(|| ca.text.cmp(&cb.text))
            .then_with(|| ca.style_id.cmp(&cb.style_id))
    });

    let mut grid = Grid::new(cell_px);
    let mut seen: HashSet<(usize, i64, i64, &str, u64)> = HashSet::new();
    // Anchors already taken by each label, for the repeat filter.
    let mut anchors: RepeatAnchors<'_> = HashMap::new();
    // Anchors of placed labels per dedupe key, for the same-value dedupe.
    let mut shown: DedupeAnchors<'_> = HashMap::new();
    let mut out: Vec<LayerOutcome> = vec![LayerOutcome::default(); layers.len()];
    for (li, i) in order {
        let c = at((li, i));
        // Dedup of identical copies (one label seen from several tiles):
        // keep the first candidate (in total order) per key.
        let (qx, qy) = c.quant();
        if !seen.insert((li, qx, qy, c.text.as_str(), c.style_id)) {
            continue;
        }
        if c.repeat_px > 0.0 {
            let taken = anchors
                .entry((li, c.text.as_str(), c.style_id))
                .or_default();
            let r2 = c.repeat_px * c.repeat_px;
            if taken.iter().any(|&(x, y)| {
                let (dx, dy) = (c.anchor_x - x, c.anchor_y - y);
                dx * dx + dy * dy < r2
            }) {
                continue;
            }
            taken.push((c.anchor_x, c.anchor_y));
        }
        // Same-value dedupe: only labels already shown count, so it is
        // checked here but recorded after the label places.
        let dedupe = c
            .dedupe
            .as_ref()
            .filter(|(k, px)| !k.is_empty() && *px > 0.0);
        if let Some((key, px)) = dedupe {
            let r2 = px * px;
            let near = shown.get(&(li, key.as_str())).is_some_and(|placed| {
                placed.iter().any(|&(x, y)| {
                    let (dx, dy) = (c.anchor_x - x, c.anchor_y - y);
                    dx * dx + dy * dy < r2
                })
            });
            if near {
                out[li].deduped.push(i);
                continue;
            }
        }
        // Collision: the first variant whose every box is free. `Always`
        // shows at the first variant without looking further; `Fallback`
        // looks, and shows at the first variant when nothing is free.
        let free = |boxes: &Vec<Aabb>| boxes.iter().all(|b| !grid.intersects_any(b));
        let first = || (Some(0), !c.variants.first().is_none_or(free));
        let (variant, overlapped) = match c.overlap {
            Overlap::Always => first(),
            Overlap::Never => (c.variants.iter().position(free), false),
            Overlap::Fallback => match c.variants.iter().position(free) {
                Some(v) => (Some(v), false),
                None => first(),
            },
        };
        let Some(variant) = variant else {
            out[li].rejected.push(i);
            continue;
        };
        out[li].placed.push(Placement {
            cand: i,
            variant,
            overlapped,
        });
        if let Some((key, _)) = dedupe {
            shown
                .entry((li, key.as_str()))
                .or_default()
                .push((c.anchor_x, c.anchor_y));
        }
        // Blocking: a shown label reserves its chosen boxes unless it ignores
        // placement (so an overlapping + ignore-placement label neither
        // collides nor blocks).
        if !c.ignore_placement {
            for b in c.variants.get(variant).into_iter().flatten() {
                grid.insert(*b);
            }
        }
    }
    out
}

/// [`place_layers`] for a lone label layer.
pub fn place(candidates: &[LabelCandidate], cell_px: f32) -> Vec<Placement> {
    place_layers(&[candidates], cell_px)
        .pop()
        .expect("one layer in, one out")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(x: f32, y: f32, half: f32) -> Aabb {
        Aabb {
            min_x: x - half,
            min_y: y - half,
            max_x: x + half,
            max_y: y + half,
        }
    }

    /// A fixed-anchor point candidate: one variant holding one box.
    fn cand(sort_key: f64, ax: i64, ay: i64, text: &str, x: f32, y: f32) -> LabelCandidate {
        LabelCandidate {
            sort_key,
            rank: PlaceRank::default(),
            world_ax: ax,
            world_ay: ay,
            text: text.into(),
            style_id: 0,
            variants: vec![vec![boxed(x, y, 10.0)]],
            anchor_x: x,
            anchor_y: y,
            repeat_px: 0.0,
            dedupe: None,
            overlap: Overlap::Never,
            ignore_placement: false,
        }
    }

    /// Placed-candidate indices, dropping the chosen-variant detail.
    fn idxs(placed: &[Placement]) -> Vec<usize> {
        placed.iter().map(|p| p.cand).collect()
    }

    #[test]
    fn aabb_intersects_excludes_touching() {
        let a = boxed(0.0, 0.0, 5.0);
        assert!(a.intersects(&boxed(8.0, 0.0, 5.0))); // overlap
        assert!(!a.intersects(&boxed(10.0, 0.0, 5.0))); // edge-touch: no
        assert!(!a.intersects(&boxed(20.0, 0.0, 5.0))); // apart
    }

    #[test]
    fn lower_sort_key_wins_overlap() {
        // Two overlapping labels; the lower sort-key places, the other drops.
        let a = {
            let mut c = cand(5.0, 100, 0, "a", 0.0, 0.0);
            c.world_ax = 100;
            c
        };
        let b = cand(1.0, 200, 0, "b", 5.0, 0.0); // overlaps a, lower key
        let placed = place(&[a, b], COLLISION_CELL_PX);
        assert_eq!(idxs(&placed), vec![1]); // b (index 1) wins
    }

    #[test]
    fn the_detailed_outcome_lists_collision_losers_but_not_duplicates() {
        let a = cand(1.0, 0, 0, "a", 0.0, 0.0);
        let b = cand(2.0, 400, 0, "b", 5.0, 0.0); // overlaps a, loses
        let dup = cand(1.0, 1, 1, "a", 0.0, 0.0); // a's own copy
        let far = cand(3.0, 4000, 0, "c", 500.0, 0.0);
        let out = place_layers_detailed(&[&[a, b, dup, far]], COLLISION_CELL_PX);
        assert_eq!(idxs(&out[0].placed), vec![0, 3]);
        assert_eq!(out[0].rejected, vec![1]);
    }

    #[test]
    fn deterministic_tiebreak_on_equal_sort_key() {
        // Equal sort-key, overlapping: tie-break by anchor then text.
        // Lower anchor-y wins.
        let a = cand(0.0, 0, 40, "z", 0.0, 10.0); // anchor y higher
        let b = cand(0.0, 0, 0, "a", 0.0, 0.0); // anchor y lower → first
        let placed = place(&[a, b], COLLISION_CELL_PX);
        assert_eq!(idxs(&placed), vec![1]);
    }

    #[test]
    fn feature_order_outranks_the_anchor_tie_break() {
        // Equal sort-key, overlapping: the feature that comes first in the
        // tile wins, even though the other sits higher on the canvas.
        let mut a = cand(0.0, 0, 40, "z", 0.0, 10.0);
        a.rank.feature = 0;
        let mut b = cand(0.0, 0, 0, "a", 0.0, 0.0); // higher on canvas
        b.rank.feature = 1;
        assert_eq!(idxs(&place(&[a, b], COLLISION_CELL_PX)), vec![0]);
        // The sort key still dominates the feature order.
        let mut a = cand(5.0, 0, 40, "z", 0.0, 10.0);
        a.rank.feature = 0;
        let mut b = cand(1.0, 0, 0, "a", 0.0, 0.0);
        b.rank.feature = 1;
        assert_eq!(idxs(&place(&[a, b], COLLISION_CELL_PX)), vec![1]);
    }

    #[test]
    fn allow_overlap_draws_both() {
        let mut a = cand(0.0, 0, 0, "a", 0.0, 0.0);
        let mut b = cand(1.0, 40, 0, "b", 5.0, 0.0);
        a.overlap = Overlap::Always;
        b.overlap = Overlap::Always;
        let placed = place(&[a, b], COLLISION_CELL_PX);
        assert_eq!(placed.len(), 2);
    }

    #[test]
    fn always_never_tries_a_later_variant() {
        // `b`'s first variant overlaps `a` and its second is clear, but
        // `Always` shows it at the first, overlapping.
        let a = cand(0.0, 0, 0, "a", 0.0, 0.0);
        let mut b = cand(1.0, 40, 0, "b", 5.0, 0.0);
        b.variants.push(vec![boxed(100.0, 0.0, 10.0)]);
        b.overlap = Overlap::Always;
        let placed = place(&[a, b], COLLISION_CELL_PX);
        let b = placed.iter().find(|p| p.cand == 1).unwrap();
        assert_eq!((b.variant, b.overlapped), (0, true));
    }

    #[test]
    fn fallback_moves_to_a_free_variant_first() {
        let a = cand(0.0, 0, 0, "a", 0.0, 0.0);
        let mut b = cand(1.0, 40, 0, "b", 5.0, 0.0);
        b.variants.push(vec![boxed(100.0, 0.0, 10.0)]);
        b.overlap = Overlap::Fallback;
        let placed = place(&[a, b], COLLISION_CELL_PX);
        let b = placed.iter().find(|p| p.cand == 1).unwrap();
        assert_eq!((b.variant, b.overlapped), (1, false));
    }

    #[test]
    fn fallback_overlaps_at_the_first_variant_when_all_are_blocked() {
        // Both of `b`'s variants overlap `a`: it places at the first anyway,
        // and reserves that box, so a later `c` on it drops.
        let a = cand(0.0, 0, 0, "a", 0.0, 0.0);
        let mut b = cand(1.0, 40, 0, "b", 5.0, 0.0);
        b.variants.push(vec![boxed(8.0, 0.0, 10.0)]);
        b.overlap = Overlap::Fallback;
        let c = cand(2.0, 80, 0, "c", 20.0, 0.0); // clear of a, on b's first box
        let placed = place(&[a, b, c], COLLISION_CELL_PX);
        assert_eq!(idxs(&placed), vec![0, 1]);
        assert_eq!((placed[1].variant, placed[1].overlapped), (0, true));
    }

    #[test]
    fn ignore_placement_does_not_block_later() {
        // A places but does not reserve its box; B overlapping A still shows.
        let mut a = cand(0.0, 0, 0, "a", 0.0, 0.0);
        a.ignore_placement = true;
        let b = cand(1.0, 40, 0, "b", 5.0, 0.0); // overlaps A
        let placed = place(&[a, b], COLLISION_CELL_PX);
        assert_eq!(idxs(&placed), vec![0, 1]);
    }

    #[test]
    fn plain_collision_still_blocks() {
        // Sanity: without allow/ignore, an overlapping later label drops.
        let a = cand(0.0, 0, 0, "a", 0.0, 0.0);
        let b = cand(1.0, 40, 0, "b", 5.0, 0.0);
        let placed = place(&[a, b], COLLISION_CELL_PX);
        assert_eq!(idxs(&placed), vec![0]);
    }

    #[test]
    fn dedup_keeps_one_per_key() {
        // The same feature (same text, anchor within a quantum) present
        // twice → one placement.
        let a = cand(0.0, 1000, 500, "town", 0.0, 0.0);
        let mut dup = cand(0.0, 1001, 501, "town", 0.0, 0.0); // +1 unit noise
        dup.text = "town".into();
        let placed = place(&[a, dup], COLLISION_CELL_PX);
        assert_eq!(placed.len(), 1);
    }

    #[test]
    fn distinct_text_same_cell_not_deduped() {
        // Same anchor cell but different text is not a duplicate.
        let a = cand(0.0, 1000, 500, "aaa", 0.0, 0.0);
        let mut b = cand(1.0, 1001, 501, "bbb", 100.0, 100.0);
        b.overlap = Overlap::Always; // avoid collision confusing the count
        let placed = place(&[a, b], COLLISION_CELL_PX);
        assert_eq!(placed.len(), 2);
    }

    #[test]
    fn variable_anchor_falls_back_on_collision() {
        // `a` occupies the primary box. `b`'s primary box overlaps `a`, but its
        // fallback anchor box is clear, so `b` places there (variant 1) instead
        // of dropping.
        let a = cand(0.0, 0, 0, "a", 0.0, 0.0);
        let mut b = cand(1.0, 40, 0, "b", 5.0, 0.0); // primary overlaps a
        b.variants.push(vec![boxed(100.0, 0.0, 10.0)]); // fallback is clear
        let placed = place(&[a, b], COLLISION_CELL_PX);
        assert_eq!(placed.len(), 2);
        let b_placed = placed.iter().find(|p| p.cand == 1).unwrap();
        assert_eq!(b_placed.variant, 1, "b should place at its fallback anchor");
    }

    #[test]
    fn variable_anchor_reserves_the_chosen_box() {
        // `b` falls back to its second anchor; a later `c` overlapping that
        // chosen box must be blocked (the reserved box is the fallback, not the
        // primary).
        let a = cand(0.0, 0, 0, "a", 0.0, 0.0);
        let mut b = cand(1.0, 40, 0, "b", 5.0, 0.0);
        b.variants.push(vec![boxed(100.0, 0.0, 10.0)]);
        let c = cand(2.0, 80, 0, "c", 100.0, 0.0); // overlaps b's fallback
        let placed = place(&[a, b, c], COLLISION_CELL_PX);
        assert_eq!(
            idxs(&placed),
            vec![0, 1],
            "c collides with b's fallback box"
        );
    }

    #[test]
    fn variable_anchor_drops_when_every_box_blocked() {
        // Both of `b`'s anchor boxes overlap `a`'s box → `b` drops entirely.
        let a = cand(0.0, 0, 0, "a", 0.0, 0.0);
        let mut b = cand(1.0, 40, 0, "b", 5.0, 0.0);
        b.variants.push(vec![boxed(8.0, 0.0, 10.0)]); // also overlaps a
        let placed = place(&[a, b], COLLISION_CELL_PX);
        assert_eq!(idxs(&placed), vec![0]);
    }

    /// A one-glyph line candidate anchored at `(x, y)` (world anchor derived
    /// from the same point so the total order follows the geometry).
    fn line_cand(text: &str, x: f32, y: f32, repeat_px: f32) -> LabelCandidate {
        LabelCandidate {
            sort_key: 0.0,
            rank: PlaceRank::default(),
            world_ax: x as i64,
            world_ay: y as i64,
            text: text.into(),
            style_id: 0,
            variants: vec![vec![boxed(x, y, 5.0)]],
            anchor_x: x,
            anchor_y: y,
            repeat_px,
            dedupe: None,
            overlap: Overlap::Always,
            ignore_placement: true,
        }
    }

    #[test]
    fn repeat_distance_drops_a_nearby_copy_of_the_same_label() {
        // Three anchors of one street name, 60 px apart, with a 125 px
        // repeat distance: the middle one is too close to the first and the
        // last is clear of the surviving anchor.
        let cands = [
            line_cand("Main St", 0.0, 0.0, 125.0),
            line_cand("Main St", 60.0, 0.0, 125.0),
            line_cand("Main St", 180.0, 0.0, 125.0),
        ];
        assert_eq!(idxs(&place(&cands, COLLISION_CELL_PX)), vec![0, 2]);
        // A different label at the same distance is unaffected.
        let mixed = [
            line_cand("Main St", 0.0, 0.0, 125.0),
            line_cand("Elm St", 60.0, 0.0, 125.0),
        ];
        assert_eq!(idxs(&place(&mixed, COLLISION_CELL_PX)), vec![0, 1]);
        // Zero repeat distance keeps every anchor (line-center placement).
        let all = [
            line_cand("Main St", 0.0, 0.0, 0.0),
            line_cand("Main St", 60.0, 0.0, 0.0),
        ];
        assert_eq!(idxs(&place(&all, COLLISION_CELL_PX)), vec![0, 1]);
        // The same street name in another layer keeps its own anchors.
        let top = [line_cand("Main St", 0.0, 0.0, 125.0)];
        let below = [line_cand("Main St", 60.0, 0.0, 125.0)];
        let placed = place_layers(&[&top, &below], COLLISION_CELL_PX);
        assert_eq!((idxs(&placed[0]), idxs(&placed[1])), (vec![0], vec![0]));
    }

    #[test]
    fn repeat_distance_is_consumed_by_a_blocked_candidate() {
        // The first candidate loses its collision but still keeps the
        // second, 60 px away, from taking its place (MapLibre records the
        // anchor at layout time, before placement runs).
        let blocker = {
            let mut c = line_cand("blocker", 0.0, 0.0, 0.0);
            c.overlap = Overlap::Never;
            c.ignore_placement = false;
            c.sort_key = -1.0;
            c
        };
        let mut a = line_cand("Main St", 0.0, 0.0, 125.0);
        a.overlap = Overlap::Never;
        let mut b = line_cand("Main St", 60.0, 0.0, 125.0);
        b.overlap = Overlap::Never;
        assert_eq!(idxs(&place(&[blocker, a, b], COLLISION_CELL_PX)), vec![0]);
    }

    /// A point candidate carrying a same-value dedupe key.
    fn keyed(sort_key: f64, key: &str, x: f32, px: f32) -> LabelCandidate {
        let mut c = cand(sort_key, x as i64 * 10, 0, "Hall", x, 0.0);
        c.dedupe = Some((key.into(), px));
        c
    }

    #[test]
    fn same_key_within_the_distance_is_deduped() {
        let out = place_layers_detailed(
            &[&[
                keyed(0.0, "Hall", 0.0, 100.0),
                keyed(1.0, "Hall", 50.0, 100.0),
            ]],
            COLLISION_CELL_PX,
        );
        assert_eq!(idxs(&out[0].placed), vec![0]);
        assert_eq!(out[0].deduped, vec![1]);
        assert!(out[0].rejected.is_empty());
    }

    #[test]
    fn same_key_beyond_the_distance_places_both() {
        let out = place_layers_detailed(
            &[&[
                keyed(0.0, "Hall", 0.0, 100.0),
                keyed(1.0, "Hall", 150.0, 100.0),
            ]],
            COLLISION_CELL_PX,
        );
        assert_eq!(idxs(&out[0].placed), vec![0, 1]);
        assert!(out[0].deduped.is_empty());
    }

    #[test]
    fn a_label_that_loses_its_collision_does_not_suppress_its_duplicate() {
        // The first `Hall` is blocked, so the second, within the distance of
        // it, must still show: otherwise the name would vanish entirely.
        let blocker = cand(-1.0, -10, 0, "blocker", 0.0, 0.0);
        let first = keyed(0.0, "Hall", 5.0, 100.0);
        let second = keyed(1.0, "Hall", 60.0, 100.0);
        let out = place_layers_detailed(&[&[blocker, first, second]], COLLISION_CELL_PX);
        assert_eq!(idxs(&out[0].placed), vec![0, 2]);
        assert_eq!(out[0].rejected, vec![1]);
        assert!(out[0].deduped.is_empty());
    }

    #[test]
    fn different_or_empty_keys_and_other_layers_are_not_deduped() {
        let out = place_layers_detailed(
            &[&[
                keyed(0.0, "Hall", 0.0, 100.0),
                keyed(1.0, "Annex", 50.0, 100.0),
            ]],
            COLLISION_CELL_PX,
        );
        assert_eq!(idxs(&out[0].placed), vec![0, 1]);
        // An empty key never dedupes.
        let out = place_layers_detailed(
            &[&[keyed(0.0, "", 0.0, 100.0), keyed(1.0, "", 50.0, 100.0)]],
            COLLISION_CELL_PX,
        );
        assert_eq!(idxs(&out[0].placed), vec![0, 1]);
        // The same key in another layer is that layer's own business.
        let top = [keyed(0.0, "Hall", 0.0, 100.0)];
        let below = [keyed(0.0, "Hall", 50.0, 100.0)];
        let out = place_layers_detailed(&[&top, &below], COLLISION_CELL_PX);
        assert_eq!(
            (idxs(&out[0].placed), idxs(&out[1].placed)),
            (vec![0], vec![0])
        );
    }

    #[test]
    fn all_glyph_boxes_of_a_line_label_must_be_free() {
        // A line label is all-or-nothing: one blocked glyph box drops the whole
        // label, and a placed one reserves every box it carries.
        let mut long = line_cand("Main St", 0.0, 0.0, 0.0);
        long.overlap = Overlap::Never;
        long.ignore_placement = false;
        long.variants = vec![vec![boxed(0.0, 0.0, 5.0), boxed(40.0, 0.0, 5.0)]];
        let mut blocker = cand(-1.0, -100, 0, "poi", 40.0, 0.0);
        blocker.world_ay = -100;
        let placed = place(&[long.clone(), blocker.clone()], COLLISION_CELL_PX);
        assert_eq!(
            idxs(&placed),
            vec![1],
            "the blocker takes the second glyph's cell, so the line label drops"
        );
        // Without the blocker the label places and reserves both boxes.
        let late = cand(2.0, 200, 0, "late", 40.0, 0.0);
        let placed = place(&[long, late], COLLISION_CELL_PX);
        assert_eq!(
            idxs(&placed),
            vec![0],
            "the reserved glyph box blocks `late`"
        );
    }

    #[test]
    fn earlier_layer_in_priority_order_wins() {
        // Cross-layer priority: the layer passed first (the one drawn on top,
        // e.g. POIs over road labels) places and knocks the other out — even
        // when the loser has the far lower `symbol-sort-key`, which never
        // crosses a layer boundary (maplibre-gl-js places symbol layers
        // top-down).
        let top = [cand(100.0, 40, 0, "poi", 5.0, 0.0)];
        let below = [cand(-100.0, 0, 0, "road", 0.0, 0.0)]; // overlaps `top`
        let placed = place_layers(&[&top, &below], COLLISION_CELL_PX);
        assert_eq!((idxs(&placed[0]), idxs(&placed[1])), (vec![0], vec![]));
        // Swapping the layer order swaps the winner.
        let placed = place_layers(&[&below, &top], COLLISION_CELL_PX);
        assert_eq!((idxs(&placed[0]), idxs(&placed[1])), (vec![0], vec![]));
    }

    #[test]
    fn layers_dedup_separately() {
        // Two layers labelling the same feature are two labels, not a
        // duplicate: with overlap allowed both place.
        let mut a = cand(0.0, 1000, 500, "Shibuya", 0.0, 0.0);
        a.overlap = Overlap::Always;
        let b = [a.clone()];
        let a = [a];
        let placed = place_layers(&[&a, &b], COLLISION_CELL_PX);
        assert_eq!(placed.iter().map(Vec::len).sum::<usize>(), 2);
    }

    #[test]
    fn order_is_frame_independent() {
        // The same window evaluated with anchors shifted by a whole tile
        // (as a neighbour tile would see them) yields the same winner set
        // — determinism across frames. We model two features; the winner
        // is chosen purely from world anchors, not the pixel positions.
        let win = |shift: i64| {
            let a = cand(2.0, 100 + shift, 0, "a", 0.0, 0.0);
            let b = cand(1.0, 100 + shift, 0, "b", 3.0, 0.0);
            place(&[a, b], COLLISION_CELL_PX)
        };
        assert_eq!(win(0), win(4096));
    }
}
