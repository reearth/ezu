//! Deterministic label collision & deduplication (MapLibre's greedy
//! screen-space placement, made tile-independent).
//!
//! MapLibre places labels greedily in screen space, "tiles nearest the
//! viewport centre first", with a global priority order. A per-tile
//! renderer can't reproduce the viewport-centre part, but everything
//! else is made **deterministic in world space** so neighbouring tiles
//! reach identical decisions and borders stay seamless:
//!
//! 1. Candidates come from the tile's own features **plus the 3×3
//!    neighbour tiles'** features (all evaluated with the same
//!    expressions), so every tile sees the same set for any label that
//!    straddles a border.
//! 2. **Dedup** — the same feature appears in several tiles (MVT buffer +
//!    neighbours). The key is `(text, quantized world anchor)`; one
//!    candidate survives per key.
//! 3. **Total order** — `(sort-key ↑, quantized anchor y, quantized
//!    anchor x, text)`. No tile-local quantity enters, so the order is
//!    identical on every tile.
//! 4. **Greedy grid insertion** — each candidate carries a collision box
//!    in a world-pixel frame; a coarse grid answers overlap queries.
//!    Winner → placed + inserted; loser → dropped. `allow_overlap`
//!    always places (and still blocks later labels, unless
//!    `ignore_placement` skips insertion).
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

/// One label placement candidate, in world-space terms only.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// MapLibre `symbol-sort-key`: lower places first. Absent = 0.
    pub sort_key: f64,
    /// World anchor in tile-extent units (exact integer:
    /// `tile_index × extent + local`), identical across the tiles that
    /// share this feature.
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
    /// Collision box in the shared world-pixel frame, already inflated by
    /// `padding-px`.
    pub aabb: Aabb,
    /// MapLibre `*-allow-overlap`: place regardless of collision.
    pub allow_overlap: bool,
    /// MapLibre `*-ignore-placement`: don't block later labels (skip
    /// inserting this box into the grid).
    pub ignore_placement: bool,
}

impl Candidate {
    /// The quantized anchor cell used for dedup and ordering (floor
    /// division so negative anchors bucket consistently).
    fn quant(&self) -> (i64, i64) {
        (
            self.world_ax.div_euclid(DEDUP_QUANTUM),
            self.world_ay.div_euclid(DEDUP_QUANTUM),
        )
    }
}

/// Deterministically dedup, order, and greedily place `candidates`.
/// Returns the indices of the placed candidates, in placement order.
///
/// Determinism: the result depends only on world-space quantities that
/// are identical across every tile sharing the 3×3 window — the total
/// order and dedup key use the quantized world anchor + text + sort key,
/// and collision boxes are in the shared world-pixel frame. No tile-local
/// input enters.
pub fn place(candidates: &[Candidate], cell_px: f32) -> Vec<usize> {
    // Total order: sort-key ↑, then quantized anchor (y, x), then text.
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by(|&a, &b| {
        let (ca, cb) = (&candidates[a], &candidates[b]);
        ca.sort_key
            .total_cmp(&cb.sort_key)
            .then_with(|| ca.quant().1.cmp(&cb.quant().1))
            .then_with(|| ca.quant().0.cmp(&cb.quant().0))
            .then_with(|| ca.text.cmp(&cb.text))
            .then_with(|| ca.style_id.cmp(&cb.style_id))
    });

    let mut grid = Grid::new(cell_px);
    let mut seen: HashSet<(i64, i64, &str, u64)> = HashSet::new();
    let mut placed = Vec::new();
    for i in order {
        let c = &candidates[i];
        // Dedup: keep the first candidate (in total order) per key.
        let (qx, qy) = c.quant();
        if !seen.insert((qx, qy, c.text.as_str(), c.style_id)) {
            continue;
        }
        // Collision: allow-overlap always shows; otherwise it must not
        // overlap an already-placed, non-ignored box.
        let shown = c.allow_overlap || !grid.intersects_any(&c.aabb);
        if !shown {
            continue;
        }
        placed.push(i);
        // Blocking: a shown label reserves its box unless it ignores
        // placement (so an allow-overlap + ignore-placement label neither
        // collides nor blocks).
        if !c.ignore_placement {
            grid.insert(c.aabb);
        }
    }
    placed
}

/// A line-placed label candidate. Unlike a point label it carries one
/// collision box *per glyph* (each already inflated by `padding-px`): the
/// label places only if every box is free, and reserves all of them —
/// MapLibre's along-line collision circles, mapped onto the AABB grid.
#[derive(Debug, Clone)]
pub struct LineCandidate {
    /// MapLibre `symbol-sort-key`: lower places first. Absent = 0.
    pub sort_key: f64,
    /// World anchor in tile-extent units (the label-centre sample point),
    /// identical across the tiles that share this line.
    pub world_ax: i64,
    pub world_ay: i64,
    /// The evaluated label text, part of the dedup key and order
    /// tie-break.
    pub text: String,
    /// Resolved label-style identity (e.g. the font stack's `font_id`);
    /// joins the dedup key and order tie-break (see [`Candidate::style_id`]).
    pub style_id: u64,
    /// Per-glyph collision boxes in the shared world-pixel frame.
    pub boxes: Vec<Aabb>,
    pub allow_overlap: bool,
    pub ignore_placement: bool,
}

impl LineCandidate {
    fn quant(&self) -> (i64, i64) {
        (
            self.world_ax.div_euclid(DEDUP_QUANTUM),
            self.world_ay.div_euclid(DEDUP_QUANTUM),
        )
    }
}

/// Deterministically dedup, order, and greedily place line-label
/// `candidates` (all-or-nothing per label). Same total order and dedup
/// key as [`place`]; a label shows only when *every* glyph box is free,
/// and then reserves all of them (unless `ignore_placement`).
pub fn place_lines(candidates: &[LineCandidate], cell_px: f32) -> Vec<usize> {
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by(|&a, &b| {
        let (ca, cb) = (&candidates[a], &candidates[b]);
        ca.sort_key
            .total_cmp(&cb.sort_key)
            .then_with(|| ca.quant().1.cmp(&cb.quant().1))
            .then_with(|| ca.quant().0.cmp(&cb.quant().0))
            .then_with(|| ca.text.cmp(&cb.text))
            .then_with(|| ca.style_id.cmp(&cb.style_id))
    });

    let mut grid = Grid::new(cell_px);
    let mut seen: HashSet<(i64, i64, &str, u64)> = HashSet::new();
    let mut placed = Vec::new();
    for i in order {
        let c = &candidates[i];
        let (qx, qy) = c.quant();
        if !seen.insert((qx, qy, c.text.as_str(), c.style_id)) {
            continue;
        }
        let shown = c.allow_overlap || c.boxes.iter().all(|b| !grid.intersects_any(b));
        if !shown {
            continue;
        }
        placed.push(i);
        if !c.ignore_placement {
            for b in &c.boxes {
                grid.insert(*b);
            }
        }
    }
    placed
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

    fn cand(sort_key: f64, ax: i64, ay: i64, text: &str, x: f32, y: f32) -> Candidate {
        Candidate {
            sort_key,
            world_ax: ax,
            world_ay: ay,
            text: text.into(),
            style_id: 0,
            aabb: boxed(x, y, 10.0),
            allow_overlap: false,
            ignore_placement: false,
        }
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
        assert_eq!(placed, vec![1]); // b (index 1) wins
    }

    #[test]
    fn deterministic_tiebreak_on_equal_sort_key() {
        // Equal sort-key, overlapping: tie-break by anchor then text.
        // Lower anchor-y wins.
        let a = cand(0.0, 0, 40, "z", 0.0, 10.0); // anchor y higher
        let b = cand(0.0, 0, 0, "a", 0.0, 0.0); // anchor y lower → first
        let placed = place(&[a, b], COLLISION_CELL_PX);
        assert_eq!(placed, vec![1]);
    }

    #[test]
    fn allow_overlap_draws_both() {
        let mut a = cand(0.0, 0, 0, "a", 0.0, 0.0);
        let mut b = cand(1.0, 40, 0, "b", 5.0, 0.0);
        a.allow_overlap = true;
        b.allow_overlap = true;
        let placed = place(&[a, b], COLLISION_CELL_PX);
        assert_eq!(placed.len(), 2);
    }

    #[test]
    fn ignore_placement_does_not_block_later() {
        // A places but does not reserve its box; B overlapping A still shows.
        let mut a = cand(0.0, 0, 0, "a", 0.0, 0.0);
        a.ignore_placement = true;
        let b = cand(1.0, 40, 0, "b", 5.0, 0.0); // overlaps A
        let placed = place(&[a, b], COLLISION_CELL_PX);
        assert_eq!(placed, vec![0, 1]);
    }

    #[test]
    fn plain_collision_still_blocks() {
        // Sanity: without allow/ignore, an overlapping later label drops.
        let a = cand(0.0, 0, 0, "a", 0.0, 0.0);
        let b = cand(1.0, 40, 0, "b", 5.0, 0.0);
        let placed = place(&[a, b], COLLISION_CELL_PX);
        assert_eq!(placed, vec![0]);
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
        b.allow_overlap = true; // avoid collision confusing the count
        let placed = place(&[a, b], COLLISION_CELL_PX);
        assert_eq!(placed.len(), 2);
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
