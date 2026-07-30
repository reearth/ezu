//! Line-placed label geometry — a port of the maplibre-gl-js
//! `symbol_placement: line` / `line-center` anchor generation and
//! glyph-along-path walk, kept pure and backend-agnostic (it needs only
//! a polyline plus per-glyph along-line offsets, never a font or a
//! canvas). The `text` node feeds it a polyline in the shared
//! world-pixel frame so two tiles walking the same line agree.
//!
//! The three primitives, all pure:
//!
//! 1. [`generate_anchors`] — candidate anchor points along the line,
//!    every `spacing` px starting half a label in (or the single arc
//!    midpoint for [`LinePlacement::LineCenter`]), each rejected if the
//!    line bends more than `max_angle_deg` within any sliding
//!    `angle_window` of the arc the label would cover.
//! 2. keep-upright — [`Anchor::reversed`] flags a label whose reading
//!    direction would run right-to-left, so the walk flips it once.
//! 3. [`place_glyphs`] — walk the line from the anchor, sampling each
//!    glyph's horizontal centre to a point + tangent angle.
//!
//! Divergences from the reference, kept deliberately simple: the
//! low-zoom spacing halving is ignored (the px value is used as-is), and
//! keep-upright is decided once per label rather than per frame (a
//! static renderer never re-flips).

/// MapLibre `symbol-placement` for a line geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinePlacement {
    /// Repeat labels every `symbol-spacing` px along the line.
    Line,
    /// One label at the line's arc-length midpoint.
    LineCenter,
}

/// One accepted anchor along a line: the arc-length of the label's
/// centre and whether the walk should be reversed to read upright.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    /// Arc-length (px) of the label centre from the polyline start.
    pub s: f32,
    /// Sample point at `s` (frame px) — the label's world anchor.
    pub x: f32,
    pub y: f32,
    /// Keep-upright: the label's reading direction runs opposite the
    /// polyline (its window points leftward), so the walk is flipped.
    pub reversed: bool,
}

/// One glyph placed along the line: the point its horizontal centre sits
/// on and the tangent angle (radians) to rotate it by.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphOnLine {
    pub x: f32,
    pub y: f32,
    /// Tangent angle in radians (`atan2(dy, dx)` of the reading
    /// direction), already flipped for a reversed (keep-upright) label.
    pub angle: f32,
}

/// Cumulative arc-lengths at each vertex (`cum[i]` = distance from the
/// start to vertex `i`); `cum.last()` is the total length. Empty for a
/// degenerate (< 2 vertex) line.
fn cumulative(poly: &[(f32, f32)]) -> Vec<f32> {
    if poly.len() < 2 {
        return Vec::new();
    }
    let mut cum = Vec::with_capacity(poly.len());
    let mut acc = 0.0f32;
    cum.push(0.0);
    for w in poly.windows(2) {
        let (dx, dy) = (w[1].0 - w[0].0, w[1].1 - w[0].1);
        acc += (dx * dx + dy * dy).sqrt();
        cum.push(acc);
    }
    cum
}

/// Point on the polyline at arc-length `s` (clamped to the ends).
fn point_at(poly: &[(f32, f32)], cum: &[f32], s: f32) -> (f32, f32) {
    let total = *cum.last().unwrap_or(&0.0);
    let s = s.clamp(0.0, total);
    for i in 0..poly.len() - 1 {
        if s <= cum[i + 1] || i + 2 == poly.len() {
            let seg = cum[i + 1] - cum[i];
            let f = if seg > 1e-6 { (s - cum[i]) / seg } else { 0.0 };
            return (
                poly[i].0 + (poly[i + 1].0 - poly[i].0) * f,
                poly[i].1 + (poly[i + 1].1 - poly[i].1) * f,
            );
        }
    }
    *poly.last().expect("poly has >= 2 vertices")
}

/// Tangent angle (radians) of the segment covering arc-length `s`.
fn tangent_at(poly: &[(f32, f32)], cum: &[f32], s: f32) -> f32 {
    for i in 0..poly.len() - 1 {
        if s <= cum[i + 1] || i + 2 == poly.len() {
            let (dx, dy) = (poly[i + 1].0 - poly[i].0, poly[i + 1].1 - poly[i].1);
            return dy.atan2(dx);
        }
    }
    0.0
}

/// Whether the line stays within `max_angle_deg` over the arc the label
/// covers (`[lo, hi]`): the turn angles of the vertices inside it are
/// summed over a window of `window` px sliding forward with each vertex,
/// and any window sum exceeding the limit rejects the anchor. A short window
/// therefore tolerates a long gentle curve but not a kink, which is what
/// MapLibre's `text-max-angle` measures.
fn max_angle_ok(
    poly: &[(f32, f32)],
    cum: &[f32],
    lo: f32,
    hi: f32,
    window: f32,
    max_angle_deg: f32,
) -> bool {
    // Turns inside the current window: (arc-length, turn in degrees).
    let mut recent: std::collections::VecDeque<(f32, f32)> = std::collections::VecDeque::new();
    let mut sum = 0.0f32;
    for i in 1..poly.len() - 1 {
        let v = cum[i];
        if v <= lo {
            continue;
        }
        if v >= hi {
            break;
        }
        let a0 = {
            let (dx, dy) = (poly[i].0 - poly[i - 1].0, poly[i].1 - poly[i - 1].1);
            dy.atan2(dx)
        };
        let a1 = {
            let (dx, dy) = (poly[i + 1].0 - poly[i].0, poly[i + 1].1 - poly[i].1);
            dy.atan2(dx)
        };
        let mut d = (a1 - a0).abs();
        if d > std::f32::consts::PI {
            d = 2.0 * std::f32::consts::PI - d;
        }
        let d = d.to_degrees();
        recent.push_back((v, d));
        sum += d;
        // Drop turns that fell out of the window behind this vertex; the
        // vertex itself always stays, so a single kink is still caught.
        while let Some(&(s0, d0)) = recent.front() {
            if v - s0 > window.max(0.0) {
                sum -= d0;
                recent.pop_front();
            } else {
                break;
            }
        }
        if sum > max_angle_deg {
            return false;
        }
    }
    true
}

/// Whether the label centred at arc-length `s` (covering `label_len`)
/// reads right-to-left in frame coordinates — keep-upright then flips it.
fn window_reversed(poly: &[(f32, f32)], cum: &[f32], s: f32, label_len: f32) -> bool {
    let a = point_at(poly, cum, s - label_len * 0.5);
    let b = point_at(poly, cum, s + label_len * 0.5);
    b.0 < a.0
}

/// Anchor-generation inputs for one label on one polyline, all in frame
/// px except the angle limit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnchorParams {
    pub placement: LinePlacement,
    /// The label's total advance.
    pub label_len: f32,
    /// MapLibre `symbol-spacing`: gap between successive line anchors
    /// (ignored for [`LinePlacement::LineCenter`]).
    pub spacing: f32,
    /// MapLibre `text-max-angle`, in degrees.
    pub max_angle_deg: f32,
    /// Arc window the bend is summed over (MapLibre: 3/5 of the font
    /// size). Zero measures each vertex's turn on its own.
    pub angle_window: f32,
}

/// Generate label anchors along `poly` (frame px).
///
/// A label shorter than the line yields at least one anchor when the
/// bend allows; a label longer than the line yields none (it can't fit).
pub fn generate_anchors(poly: &[(f32, f32)], p: &AnchorParams) -> Vec<Anchor> {
    let cum = cumulative(poly);
    let Some(&total) = cum.last() else {
        return Vec::new();
    };
    let half = p.label_len * 0.5;
    // A label longer than the line can never fit.
    if p.label_len > total || total <= 0.0 {
        return Vec::new();
    }
    let mut anchors = Vec::new();
    let push_if_straight = |s: f32, anchors: &mut Vec<Anchor>| {
        if !max_angle_ok(
            poly,
            &cum,
            s - half,
            s + half,
            p.angle_window,
            p.max_angle_deg,
        ) {
            return;
        }
        let (x, y) = point_at(poly, &cum, s);
        anchors.push(Anchor {
            s,
            x,
            y,
            reversed: window_reversed(poly, &cum, s, p.label_len),
        });
    };
    match p.placement {
        LinePlacement::LineCenter => push_if_straight(total * 0.5, &mut anchors),
        LinePlacement::Line => {
            // MapLibre widens the requested spacing so successive labels
            // keep at least a quarter-spacing gap between their ends.
            let mut spacing = p.spacing;
            if spacing - p.label_len < spacing * 0.25 {
                spacing = p.label_len + spacing * 0.25;
            }
            let step = spacing.max(1.0);
            // First anchor half a label in, then every `spacing`, while the
            // whole label still fits within the line.
            let mut s = half;
            while s <= total - half + 1e-3 {
                push_if_straight(s, &mut anchors);
                s += step;
            }
        }
    }
    anchors
}

/// Walk `poly` from `anchor`, placing each glyph at its horizontal
/// centre. `centre_offsets` are each glyph's centre offset (px) from the
/// label centre (positive = forward in reading order). Returns `None`
/// when any glyph would fall off either end of the line.
pub fn place_glyphs(
    poly: &[(f32, f32)],
    anchor: &Anchor,
    centre_offsets: &[f32],
) -> Option<Vec<GlyphOnLine>> {
    let cum = cumulative(poly);
    let &total = cum.last()?;
    let mut out = Vec::with_capacity(centre_offsets.len());
    for &off in centre_offsets {
        // Reading direction advances with the polyline unless reversed.
        let s = if anchor.reversed {
            anchor.s - off
        } else {
            anchor.s + off
        };
        if s < -1e-3 || s > total + 1e-3 {
            return None;
        }
        let (x, y) = point_at(poly, &cum, s);
        let mut angle = tangent_at(poly, &cum, s);
        if anchor.reversed {
            angle += std::f32::consts::PI;
        }
        // Normalize to (-π, π] so a flipped tangent reads as its canonical
        // upright angle rather than a wrapped-around value.
        let tau = 2.0 * std::f32::consts::PI;
        angle = (angle + std::f32::consts::PI).rem_euclid(tau) - std::f32::consts::PI;
        out.push(GlyphOnLine { x, y, angle });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A straight horizontal line of length `len` starting at the origin.
    fn straight(len: f32) -> Vec<(f32, f32)> {
        vec![(0.0, 0.0), (len, 0.0)]
    }

    /// Anchor params with a generous angle window, so a test that isn't
    /// about curvature measures the bend over the whole label.
    fn params(
        placement: LinePlacement,
        label_len: f32,
        spacing: f32,
        max_angle_deg: f32,
    ) -> AnchorParams {
        AnchorParams {
            placement,
            label_len,
            spacing,
            max_angle_deg,
            angle_window: f32::MAX,
        }
    }

    #[test]
    fn line_anchors_are_spaced_from_half_a_label() {
        // A 20 px label on a 100 px line at 30 px spacing: first anchor at
        // half the label (10), then +30 while it still fits (10, 40, 70).
        let a = generate_anchors(
            &straight(100.0),
            &params(LinePlacement::Line, 20.0, 30.0, 45.0),
        );
        let ss: Vec<f32> = a.iter().map(|a| a.s).collect();
        assert_eq!(ss, vec![10.0, 40.0, 70.0]);
        // Each anchor sits on the line at (s, 0).
        assert!(a.iter().all(|a| (a.y).abs() < 1e-3));
        assert!((a[1].x - 40.0).abs() < 1e-3);
    }

    #[test]
    fn label_longer_than_line_is_dropped() {
        let a = generate_anchors(
            &straight(30.0),
            &params(LinePlacement::Line, 40.0, 20.0, 45.0),
        );
        assert!(a.is_empty(), "a label that can't fit yields no anchors");
        let c = generate_anchors(
            &straight(30.0),
            &params(LinePlacement::LineCenter, 40.0, 20.0, 45.0),
        );
        assert!(c.is_empty());
    }

    #[test]
    fn line_center_places_exactly_one_at_the_midpoint() {
        let a = generate_anchors(
            &straight(80.0),
            &params(LinePlacement::LineCenter, 20.0, 30.0, 45.0),
        );
        assert_eq!(a.len(), 1);
        assert!((a[0].s - 40.0).abs() < 1e-3);
    }

    #[test]
    fn max_angle_rejects_a_sharp_corner_but_passes_a_gentle_curve() {
        // A right-angle bend at the midpoint: two 50 px legs meeting at 90°.
        let sharp = vec![(0.0, 0.0), (50.0, 0.0), (50.0, 50.0)];
        // A label long enough that its window spans the corner is rejected.
        let a = generate_anchors(&sharp, &params(LinePlacement::LineCenter, 60.0, 30.0, 45.0));
        assert!(a.is_empty(), "a 90° corner exceeds the 45° max angle");
        // A gentle ~10° bend of the same geometry passes.
        let gentle = vec![(0.0, 0.0), (50.0, 0.0), (100.0, 9.0)];
        let b = generate_anchors(
            &gentle,
            &params(LinePlacement::LineCenter, 60.0, 30.0, 45.0),
        );
        assert_eq!(b.len(), 1, "a gentle bend is within the max angle");
    }

    #[test]
    fn max_angle_is_measured_over_a_sliding_window() {
        // A quarter circle sampled every 10°: 90° of total bend, but never
        // more than ~20° within any 20 px of arc.
        let arc: Vec<(f32, f32)> = (0..=9)
            .map(|i| {
                let t = (i as f32) * 10.0f32.to_radians();
                (100.0 * t.sin(), 100.0 * (1.0 - t.cos()))
            })
            .collect();
        let mut p = params(LinePlacement::LineCenter, 100.0, 250.0, 45.0);
        p.angle_window = 20.0;
        assert_eq!(
            generate_anchors(&arc, &p).len(),
            1,
            "a long gentle curve passes: no window exceeds the limit"
        );
        // The same span with a kink instead of a curve is rejected even
        // though the total bend is smaller.
        let kink = vec![(0.0, 0.0), (60.0, 0.0), (60.0, 60.0), (120.0, 60.0)];
        assert!(generate_anchors(&kink, &p).is_empty());
    }

    #[test]
    fn spacing_widens_to_keep_a_gap_between_long_labels() {
        // A 220 px label at 250 px spacing would leave only 30 px between
        // ends, so the step grows to label + spacing/4 = 282.5.
        let a = generate_anchors(
            &straight(1000.0),
            &params(LinePlacement::Line, 220.0, 250.0, 45.0),
        );
        let ss: Vec<f32> = a.iter().map(|a| a.s).collect();
        assert_eq!(ss, vec![110.0, 392.5, 675.0]);
    }

    #[test]
    fn keep_upright_flags_a_right_to_left_line() {
        // A line running right-to-left (decreasing x) reads backwards, so
        // its anchor is flagged reversed; a left-to-right line is not.
        let rtl = vec![(100.0, 0.0), (0.0, 0.0)];
        let a = generate_anchors(&rtl, &params(LinePlacement::LineCenter, 20.0, 30.0, 45.0));
        assert!(a[0].reversed, "a right-to-left line is flipped upright");
        let ltr = straight(100.0);
        let b = generate_anchors(&ltr, &params(LinePlacement::LineCenter, 20.0, 30.0, 45.0));
        assert!(!b[0].reversed);
    }

    #[test]
    fn glyph_walk_follows_the_tangent_of_a_straight_line() {
        // Three glyph centres at -10, 0, +10 from a label anchored at 50 on
        // a horizontal line: they land at x = 40, 50, 60, all angle 0.
        let a = &generate_anchors(
            &straight(100.0),
            &params(LinePlacement::LineCenter, 20.0, 30.0, 45.0),
        )[0];
        let g = place_glyphs(&straight(100.0), a, &[-10.0, 0.0, 10.0]).unwrap();
        assert!((g[0].x - 40.0).abs() < 1e-3 && g[0].angle.abs() < 1e-3);
        assert!((g[1].x - 50.0).abs() < 1e-3);
        assert!((g[2].x - 60.0).abs() < 1e-3);
        assert!(g.iter().all(|g| g.y.abs() < 1e-3));
    }

    #[test]
    fn glyph_walk_rotates_to_a_diagonal_tangent() {
        // A 45° diagonal: every glyph's tangent angle is π/4.
        let diag = vec![(0.0, 0.0), (100.0, 100.0)];
        let a = &generate_anchors(&diag, &params(LinePlacement::LineCenter, 20.0, 30.0, 90.0))[0];
        let g = place_glyphs(&diag, a, &[-10.0, 0.0, 10.0]).unwrap();
        assert!(g
            .iter()
            .all(|g| (g.angle - std::f32::consts::FRAC_PI_4).abs() < 1e-3));
    }

    #[test]
    fn reversed_walk_reads_leftward_and_flips_angle() {
        // A right-to-left line: forward reading order advances toward
        // decreasing x, and the glyph angle is flipped by π so text is
        // upright.
        let rtl = vec![(100.0, 0.0), (0.0, 0.0)];
        let a = &generate_anchors(&rtl, &params(LinePlacement::LineCenter, 20.0, 30.0, 45.0))[0];
        let g = place_glyphs(&rtl, a, &[-10.0, 10.0]).unwrap();
        // Anchor is at x = 50; reading order runs opposite the polyline so
        // it reads left-to-right: a later glyph (offset +10) lands at the
        // larger x (60), an earlier one (offset -10) at the smaller x (40).
        assert!((g[0].x - 40.0).abs() < 1e-3);
        assert!((g[1].x - 60.0).abs() < 1e-3);
        // The leftward polyline tangent (π), flipped, reads rightward (≈ 0).
        assert!(g[0].angle.abs() < 1e-3);
    }
}
