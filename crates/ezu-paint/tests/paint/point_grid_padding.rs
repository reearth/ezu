//! `point-grid` covers the padded canvas, not just the visible tile:
//! a disk (or sprite) centred in the margin has to spill into the tile,
//! or every tile border shows a seam.

use crate::common::render_tile;
use ezu_graph::TileId;

const SIZE: u32 = 256;
const PAD: u32 = 16;
const EXTENT: f64 = 4096.0;
/// Extent units per rendered pixel, for `tile-size` 256 at extent 4096.
const UNITS_PER_PX: f64 = EXTENT / SIZE as f64;

fn style(anchor: &str, spacing_x: f64, offset_x: f64) -> String {
    format!(
        r##"{{
      "name": "grid",
      "tile-size": {SIZE},
      "nodes": {{
        "bg":   {{ "op": "solid", "color": "#ffffff" }},
        "pts":  {{ "op": "point-grid", "spacing": {spacing_x},
                   "spacing-y": 4096, "offset-x": {offset_x}, "offset-y": 2048,
                   "anchor": "{anchor}" }},
        "dots": {{ "op": "circles", "features": "@pts", "radius": 6, "color": "#000000" }},
        "out":  {{ "op": "blend", "base": "@bg", "over": "@dots" }}
      }},
      "output": "@out"
    }}"##
    )
}

fn is_dark(px: [u8; 4]) -> bool {
    px[0] < 128 && px[1] < 128 && px[2] < 128
}

/// The row every lattice point lands on: `offset-y` 2048 of 4096, at
/// `tile-size` 256.
const ROW: u32 = 128;

/// Centres of the runs of dark pixels along `row`.
fn run_centers(row: &[bool]) -> Vec<f64> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (x, &dark) in row.iter().enumerate() {
        match (dark, start) {
            (true, None) => start = Some(x),
            (false, Some(s)) => {
                out.push((s + x - 1) as f64 / 2.0);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s + row.len() - 1) as f64 / 2.0);
    }
    out
}

/// The tile's own pixels along [`ROW`]. `render_tile` hands back the
/// padded buffer, so the tile starts `PAD` px in on both axes.
fn dark_row(json: &str, tile: TileId) -> Vec<bool> {
    let r = render_tile(json, SIZE, PAD, tile);
    assert_eq!(r.width, SIZE + 2 * PAD, "expected a padded buffer");
    (0..SIZE)
        .map(|x| is_dark(r.pixel(PAD + x, PAD + ROW)))
        .collect()
}

/// A lattice point 4 px outside the left edge still paints the sliver of
/// its disk that reaches into the tile.
#[test]
fn point_in_the_margin_spills_into_the_tile() {
    // `offset-x` -4 px in extent units, so the point sits at px -4 with a
    // 6 px radius: two columns of it are visible.
    let row = dark_row(
        &style("tile", 4096.0, -4.0 * UNITS_PER_PX),
        TileId { z: 0, x: 0, y: 0 },
    );
    assert!(
        row[0] && row[1],
        "the disk centred in the left margin did not reach the tile"
    );
    assert!(
        !row[3],
        "a 6 px disk at px -4 should stop before column 3; got a wider blob"
    );
}

/// World anchoring is untouched: widening the lattice adds indices, it
/// never moves a point. Adjacent tiles agree on where every point is,
/// including the one straddling their shared edge.
#[test]
fn world_lattice_agrees_across_a_tile_seam() {
    // A pitch that divides neither the extent nor the tile, and an offset
    // putting a point 1.25 px past the seam between the two tiles — the
    // point that only exists if the lattice covers the margin.
    let spacing = 1500.0;
    let offset = 616.0;
    let (z, tx, ty) = (13, 4000, 3000);
    let json = style("world", spacing, offset);

    let mut row = dark_row(&json, TileId { z, x: tx, y: ty });
    row.extend(dark_row(
        &json,
        TileId {
            z,
            x: tx + 1,
            y: ty,
        },
    ));

    // Where the world lattice says the points are, in the same combined
    // pixel coordinates.
    let origin = tx as f64 * EXTENT;
    let expected: Vec<f64> = (0..)
        .map(|k| (offset + k as f64 * spacing - origin) / UNITS_PER_PX)
        .skip_while(|&px| px < 0.0)
        .take_while(|&px| px < 2.0 * SIZE as f64)
        .collect();
    // Runs touching either end of the combined row are cut off, so their
    // centre says nothing; compare the ones fully inside.
    let inside = |px: &f64| *px > 8.0 && *px < 2.0 * SIZE as f64 - 8.0;
    let want: Vec<f64> = expected.into_iter().filter(inside).collect();
    let got: Vec<f64> = run_centers(&row).into_iter().filter(inside).collect();

    assert_eq!(
        got.len(),
        want.len(),
        "expected {want:?} lattice points across the seam, found {got:?}"
    );
    for (g, w) in got.iter().zip(&want) {
        assert!(
            (g - w).abs() <= 1.0,
            "point at {w} px rendered at {g} px; got {got:?}, want {want:?}"
        );
    }
}
