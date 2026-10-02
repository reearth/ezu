//! `flow-smear` — line integral convolution along a direction raster.
//! The input and the field are bound as host rasters, so each test
//! states exactly which pixel is bright and which way the field points.

use crate::common::{render_tile, render_with_rasters};
use ezu_graph::{build_graph, RasterBuf, TileId};
use ezu_paint::nodes::default_registry;
use ezu_style::Document;

const TILE: TileId = TileId { z: 0, x: 0, y: 0 };
const SIZE: u32 = 32;

/// `img` and `dir` bound to `flow-smear`'s `input` and `field`; `extra`
/// is spliced into the flow-smear node.
fn doc(extra: &str) -> String {
    format!(
        r##"{{
          "name": "flow-smear-test",
          "tile-size": {SIZE},
          "sources": {{
            "img": {{ "type": "raster", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}.png" }},
            "dir": {{ "type": "raster", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}.png" }}
          }},
          "nodes": {{
            "img": {{ "op": "raster", "source": "img" }},
            "dir": {{ "op": "raster", "source": "dir" }},
            "out": {{ "op": "flow-smear", "input": "@img", "field": "@dir"{extra} }}
          }},
          "output": "@out"
        }}"##
    )
}

/// Opaque black with one white pixel at `(dx, dy)`.
fn dot(dx: u32, dy: u32) -> RasterBuf {
    let mut buf = RasterBuf::new(SIZE, SIZE);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let v = if (x, y) == (dx, dy) { 255 } else { 0 };
            let i = ((y * SIZE + x) * 4) as usize;
            buf.pixels[i..i + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    buf
}

/// A direction raster in `flow-field`'s encoding, from `f(x, y)`.
fn field(f: impl Fn(f32, f32) -> (f32, f32)) -> RasterBuf {
    let enc = |v: f32| ((0.5 + 0.5 * v) * 255.0).round().clamp(0.0, 255.0) as u8;
    let mut buf = RasterBuf::new(SIZE, SIZE);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (vx, vy) = f(x as f32, y as f32);
            let i = ((y * SIZE + x) * 4) as usize;
            buf.pixels[i..i + 4].copy_from_slice(&[enc(vx), enc(vy), 0, 255]);
        }
    }
    buf
}

fn smear(extra: &str, img: RasterBuf, dir: RasterBuf) -> std::sync::Arc<RasterBuf> {
    render_with_rasters(&doc(extra), SIZE, 0, TILE, &[("img", img), ("dir", dir)])
}

fn grey(r: &RasterBuf, x: u32, y: u32) -> u8 {
    r.pixel(x, y)[0]
}

#[test]
fn uniform_field_smears_a_dot_into_a_line() {
    // Length 5 each way: 11 samples per pixel, so every pixel within 5
    // of the dot along the row sees it once, at about 255 / 11.
    let r = smear(r#", "length-px": 5"#, dot(16, 16), field(|_, _| (1.0, 0.0)));
    for x in 11..=21 {
        let v = grey(&r, x, 16);
        assert!((18..=28).contains(&v), "x={x} on the line: {v}");
    }
    for x in [9, 10, 22, 23] {
        assert!(grey(&r, x, 16) < 3, "x={x} past the ends must stay dark");
    }
    // One pixel tall: the rows either side see at most a trace of the
    // field's 8-bit rounding off the axis.
    for x in 11..=21 {
        for y in [15, 17] {
            assert!(grey(&r, x, y) < 3, "({x}, {y}) off the line");
        }
    }
    assert_eq!(r.pixel(16, 16)[3], 255, "alpha stays opaque");
}

#[test]
fn forward_smears_only_downstream() {
    let r = smear(
        r#", "length-px": 5, "sides": "forward""#,
        dot(16, 16),
        field(|_, _| (1.0, 0.0)),
    );
    for x in 16..=21 {
        assert!(grey(&r, x, 16) > 30, "x={x} downstream should be lit");
    }
    for x in 10..16 {
        assert!(grey(&r, x, 16) < 3, "x={x} upstream must stay dark");
    }
}

#[test]
fn zero_field_is_the_identity() {
    let img = dot(16, 16);
    let r = smear(r#", "length-px": 8"#, img.clone(), field(|_, _| (0.0, 0.0)));
    assert_eq!(r.pixels, img.pixels);
}

#[test]
fn smooth_taper_fades_the_ends() {
    let ratio = |extra: &str| {
        let r = smear(extra, dot(16, 16), field(|_, _| (1.0, 0.0)));
        grey(&r, 20, 16) as f32 / grey(&r, 16, 16) as f32
    };
    let flat = ratio(r#", "length-px": 5"#);
    let smooth = ratio(r#", "length-px": 5, "taper": "smooth""#);
    assert!((flat - 1.0).abs() < 0.1, "flat weights alike: {flat}");
    assert!(
        smooth < 0.5 * flat,
        "smooth should fade the end: {smooth} vs {flat}"
    );
}

#[test]
fn walk_follows_a_curving_field() {
    // Counter-clockwise circles around (16, 20), the dot on the one of
    // radius 10, at its top where the field points along -x. The smear
    // must bend down the circle, not carry straight on along row 10.
    let (cx, cy) = (16.0, 20.0);
    let circle = |x: f32, y: f32| {
        let (rx, ry) = (x - cx, y - cy);
        let r = (rx * rx + ry * ry).sqrt().max(1e-3);
        (ry / r, -rx / r)
    };
    let r = smear(r#", "length-px": 9"#, dot(16, 10), field(circle));
    // About 8 px of arc from the top: (16 ± 7.2, 20 − 7.0).
    for x in [9u32, 23] {
        assert!(grey(&r, x, 13) > 5, "({x}, 13) on the circle should be lit");
        assert!(
            grey(&r, x, 10) < 3,
            "({x}, 10) on the tangent must stay dark"
        );
    }
}

#[test]
fn unknown_enums_are_rejected() {
    for extra in [r#", "sides": "back""#, r#", "taper": "sharp""#] {
        let doc = Document::from_json(&doc(extra)).expect("parse");
        assert!(
            build_graph(&doc, &default_registry()).is_err(),
            "{extra} must fail at build"
        );
    }
}

#[test]
fn length_bounds_the_pad() {
    let json = |length: &str| {
        format!(
            r##"{{
              "name": "flow-smear-test",
              "tile-size": {SIZE},
              "nodes": {{
                "img": {{ "op": "solid", "color": "#808080" }},
                "z":   {{ "op": "zoom" }},
                "out": {{ "op": "flow-smear", "input": "@img", "field": "@img"{length} }}
              }},
              "output": "@out"
            }}"##
        )
    };
    let pad = |length: &str| {
        let doc = Document::from_json(&json(length)).expect("parse");
        build_graph(&doc, &default_registry()).map(|g| g.required_pad().unwrap())
    };
    // The walk's reach plus one pixel for the bilinear read at its end.
    assert_eq!(pad(r#", "length-px": 6.5"#).unwrap(), 8);
    assert_eq!(pad("").unwrap(), 13, "default length 12");
    // An @node length has no value until the tile renders, so the pad
    // comes from a declared ceiling, and without one the build fails.
    assert!(pad(r#", "length-px": "@z""#).is_err());
    assert_eq!(
        pad(r#", "length-px": "@z", "length-px-max": 8"#).unwrap(),
        9
    );
}

#[test]
fn world_anchored_smear_is_seamless_across_adjacent_tiles() {
    // World-anchored noise smeared along a world-anchored noise field:
    // where two padded tiles overlap, every pixel whose walk stays inside
    // both canvases must come out byte-identical.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "src": { "op": "noise", "type": "perlin", "scale-px": 6, "seed": 11 },
        "dir": { "op": "noise", "type": "perlin", "scale-px": 20, "seed": 23 },
        "out": { "op": "flow-smear", "input": "@src", "field": "@dir", "length-px": 5 }
      },
      "output": "@out"
    }"##;
    let (tile, pad, reach) = (32u32, 12u32, 6u32);
    let left = render_tile(json, tile, pad, TileId { z: 4, x: 5, y: 7 });
    let right = render_tile(json, tile, pad, TileId { z: 4, x: 6, y: 7 });
    // The two canvases share 2·pad columns around the border; a walk
    // needs `reach` px of margin from either canvas edge.
    let shared = tile..(tile + 2 * pad);
    let mut compared = 0;
    for lx in shared {
        let rx = lx - tile;
        if lx + reach >= tile + 2 * pad || rx < reach {
            continue;
        }
        for y in reach..(tile + 2 * pad - reach) {
            assert_eq!(
                left.pixel(lx, y),
                right.pixel(rx, y),
                "seam at x={lx} y={y}"
            );
            compared += 1;
        }
    }
    assert!(compared > 0);
}
