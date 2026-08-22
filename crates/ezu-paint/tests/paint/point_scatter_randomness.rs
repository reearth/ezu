//! `point-scatter` has to be genuinely random, and that is a measurable
//! claim, so this file measures it instead of eyeballing a render.
//!
//! The thing being defended is why the op exists at all. Jittering a
//! `point-grid` does not remove the grid: one point per cell means the count
//! per cell has *zero* variance, so the density stays perfectly even however
//! far each point is nudged, and the pitch is still there to be found. Two
//! independent measurements are taken here, both against a `point-grid`
//! control run through the same code — a test that only reports a small
//! number for the new op proves nothing about the measurement.
//!
//! - Per-cell counts: a lattice gives variance 0, a Poisson process gives
//!   variance equal to the mean. `point-scatter` has to look Poisson.
//! - The spectrum at the cell pitch: a lattice puts a towering line there,
//!   and `point-scatter` must leave the frequency indistinguishable from its
//!   neighbours.

use crate::common::render_tile;
use ezu_graph::TileId;

const SIZE: u32 = 1024;
const PAD: u32 = 16;
const EXTENT: f64 = 4096.0;
/// Extent units per rendered pixel at `tile-size` 1024, extent 4096.
const UNITS_PER_PX: f64 = EXTENT / SIZE as f64;
/// Mean spacing in pixels. Divides `SIZE`, and `EXTENT` is a whole number of
/// cells, so the cells line up with the tile corner and 1024 of them sit
/// fully inside — no partial cell to special-case.
const PITCH_PX: u32 = 32;
const CELLS: u32 = SIZE / PITCH_PX;
const TILE: TileId = TileId {
    z: 12,
    x: 2000,
    y: 1300,
};

/// A tile of dots, one pixel across so that two points in one cell stay two
/// separate blobs.
///
/// `extra` carries whatever the caller wants to add to the source node: a
/// `seed`, or the half-cell offset that puts the `point-grid` control's
/// points at cell centres rather than on the cell corners, where rounding
/// would make which cell they belong to a coin toss.
fn style(op: &str, extra: &str) -> String {
    let spacing = PITCH_PX as f64 * UNITS_PER_PX;
    format!(
        r##"{{
      "name": "scatter",
      "tile-size": {SIZE},
      "nodes": {{
        "bg":   {{ "op": "solid", "color": "#ffffff" }},
        "pts":  {{ "op": "{op}", "spacing": {spacing}, "anchor": "world"{extra} }},
        "dots": {{ "op": "circles", "features": "@pts", "radius": 1, "color": "#000000" }},
        "out":  {{ "op": "blend", "base": "@bg", "over": "@dots" }}
      }},
      "output": "@out"
    }}"##
    )
}

/// Half a cell, as the `offset-x` / `offset-y` a `point-grid` needs to sit at
/// cell centres.
fn half_cell_offset() -> String {
    let h = PITCH_PX as f64 * UNITS_PER_PX / 2.0;
    format!(r#", "offset-x": {h}, "offset-y": {h}"#)
}

/// Centroids of the connected blobs of dark pixels in the tile interior —
/// one per drawn point, except for the rare pair that lands close enough to
/// touch.
fn point_positions(op: &str, extra: &str, tile: TileId) -> Vec<(f64, f64)> {
    let r = render_tile(&style(op, extra), SIZE, PAD, tile);
    let n = SIZE as usize;
    let mut dark: Vec<bool> = (0..n * n)
        .map(|i| r.pixel(PAD + (i % n) as u32, PAD + (i / n) as u32)[0] < 200)
        .collect();
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..n * n {
        if !std::mem::replace(&mut dark[start], false) {
            continue;
        }
        stack.push(start);
        let (mut sx, mut sy, mut area) = (0.0, 0.0, 0.0);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % n, i / n);
            sx += x as f64;
            sy += y as f64;
            area += 1.0;
            for (dx, dy) in [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)] {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if (0..n as i64).contains(&nx) && (0..n as i64).contains(&ny) {
                    let j = ny as usize * n + nx as usize;
                    if std::mem::replace(&mut dark[j], false) {
                        stack.push(j);
                    }
                }
            }
        }
        out.push((sx / area, sy / area));
    }
    out
}

/// Mean and variance of the number of points per cell over the tile
/// interior. For a Poisson process the two are equal; for a lattice the
/// variance is 0.
fn count_moments(op: &str, extra: &str, tile: TileId) -> (f64, f64) {
    let mut counts = vec![0u32; (CELLS * CELLS) as usize];
    for (x, y) in point_positions(op, extra, tile) {
        let (cx, cy) = (x as u32 / PITCH_PX, y as u32 / PITCH_PX);
        if cx < CELLS && cy < CELLS {
            counts[(cy * CELLS + cx) as usize] += 1;
        }
    }
    let n = counts.len() as f64;
    let mean = counts.iter().map(|&c| c as f64).sum::<f64>() / n;
    let var = counts
        .iter()
        .map(|&c| (c as f64 - mean).powi(2))
        .sum::<f64>()
        / n;
    (mean, var)
}

/// An exact lattice puts exactly one point in every cell.
#[test]
fn a_lattice_has_no_variance_in_its_per_cell_counts() {
    let (mean, var) = count_moments("point-grid", &half_cell_offset(), TILE);
    assert!(
        (mean - 1.0).abs() < 1e-9 && var < 1e-9,
        "the control is not a clean lattice: mean {mean}, variance {var}"
    );
}

/// And `point-scatter` puts a *variable* number in each, with the variance a
/// Poisson process of the same density would have — which is what gives the
/// pattern its clumps and gaps. The op's per-cell distribution is built to
/// match Poisson's first two moments exactly, so both numbers are 1 up to
/// sampling error over the 1024 cells measured, plus a per-cent or so of
/// under-count from pairs of points that land close enough to merge into one
/// blob.
#[test]
fn per_cell_counts_match_the_poisson_expectation() {
    for seed in ["", r#", "seed": 1"#, r#", "seed": 7"#] {
        let (mean, var) = count_moments("point-scatter", seed, TILE);
        assert!(
            (0.88..1.12).contains(&mean),
            "density drifted off one point per cell (seed `{seed}`): mean {mean}"
        );
        assert!(
            (0.7..1.35).contains(&var),
            "per-cell counts are not Poisson-like (seed `{seed}`): \
             variance {var}, expected ~1 — a fixed-count lattice gives 0"
        );
    }
}

/// Per-column and per-row ink totals over the visible tile, ink being
/// `1 - luma` so white paper contributes nothing.
fn ink_profiles(op: &str, extra: &str, tile: TileId) -> (Vec<f64>, Vec<f64>) {
    let r = render_tile(&style(op, extra), SIZE, PAD, tile);
    let n = SIZE as usize;
    let mut cols = vec![0.0; n];
    let mut rows = vec![0.0; n];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let px = r.pixel(PAD + x, PAD + y);
            let ink = 1.0 - (px[0] as f64 + px[1] as f64 + px[2] as f64) / (3.0 * 255.0);
            cols[x as usize] += ink;
            rows[y as usize] += ink;
        }
    }
    (cols, rows)
}

/// Magnitude of the DFT of `signal` at bin `k` (cycles across the whole
/// signal), with the mean removed so the DC term does not leak in. One
/// frequency at a time is all this needs, and that is a few lines — cheaper
/// than taking on an FFT dependency.
fn dft_magnitude(signal: &[f64], k: f64) -> f64 {
    let n = signal.len() as f64;
    let mean = signal.iter().sum::<f64>() / n;
    let (mut re, mut im) = (0.0, 0.0);
    for (i, &v) in signal.iter().enumerate() {
        let phase = -2.0 * std::f64::consts::PI * k * i as f64 / n;
        re += (v - mean) * phase.cos();
        im += (v - mean) * phase.sin();
    }
    re.hypot(im)
}

/// How far the cell pitch stands above the rest of the spectrum: its
/// magnitude over the median magnitude of the bins around it. 1 means the
/// pitch is no more present than noise; an exact lattice scores in the
/// thousands.
///
/// The floor is a *local* median rather than one taken across the whole
/// spectrum because a dot has width: painting discs instead of single pixels
/// rolls the high frequencies off, so the median over every bin sits well
/// below the noise level near the frequency of interest and would flatter
/// any signal there.
fn pitch_prominence(signal: &[f64]) -> f64 {
    let n = signal.len();
    let k_pitch = n as f64 / PITCH_PX as f64;
    let neighbourhood: Vec<f64> = (1..=n / 2)
        .filter(|&k| {
            let d = (k as f64 - k_pitch).abs();
            // Skip the pitch bin and the two either side, so a real line
            // does not raise its own reference level.
            (2.0..=24.0).contains(&d)
        })
        .map(|k| dft_magnitude(signal, k as f64))
        .collect();
    let mut sorted = neighbourhood;
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let floor = sorted[sorted.len() / 2];
    assert!(floor > 0.0, "the spectrum is empty — nothing was drawn");
    dft_magnitude(signal, k_pitch) / floor
}

/// The measurement, checked against the case it exists to catch. Without
/// this the assertion below would be worthless.
#[test]
fn the_spectrum_measurement_sees_a_lattice_when_there_is_one() {
    let (cols, rows) = ink_profiles("point-grid", "", TILE);
    let (px, py) = (pitch_prominence(&cols), pitch_prominence(&rows));
    assert!(
        px > 1e3 && py > 1e3,
        "a point-grid should show a towering line at its pitch; got x {px:.1}, y {py:.1}"
    );
}

/// `point-scatter` leaves no such line: the cell pitch is indistinguishable
/// from the noise around it, on both axes.
#[test]
fn scatter_leaves_no_energy_at_the_cell_pitch() {
    let (cols, rows) = ink_profiles("point-scatter", "", TILE);
    let (px, py) = (pitch_prominence(&cols), pitch_prominence(&rows));
    assert!(
        px < 3.0 && py < 3.0,
        "point-scatter still carries its cell pitch; got x {px:.2}, y {py:.2}"
    );
}

/// World anchoring: the strip of world covered by both a tile's right margin
/// and its neighbour's left edge has to render identically from either tile,
/// or the scatter tears at every seam.
#[test]
fn adjacent_tiles_agree_on_the_world_they_share() {
    let json = style("point-scatter", "");
    let left = render_tile(&json, SIZE, PAD, TILE);
    let right = render_tile(
        &json,
        SIZE,
        PAD,
        TileId {
            z: TILE.z,
            x: TILE.x + 1,
            y: TILE.y,
        },
    );
    // The left tile's right margin covers the same world as the first `PAD`
    // pixels of the right tile.
    let mut ink = 0.0;
    for y in 0..left.height {
        for d in 0..PAD {
            let a = left.pixel(PAD + SIZE + d, y);
            let b = right.pixel(PAD + d, y);
            assert_eq!(
                a, b,
                "the shared strip disagrees at margin column {d}, row {y}: {a:?} vs {b:?}"
            );
            ink += 1.0 - (a[0] as f64) / 255.0;
        }
    }
    // Not the trivial agreement of two blank strips.
    assert!(ink > 5.0, "the shared strip is blank, so it proves nothing");
}
