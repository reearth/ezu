//! Gradient nodes: linear, radial, conic, diamond.

use crate::common::render;

#[test]
fn gradient_linear_top_to_bottom() {
    // Vertical black-to-white gradient. Top row is black, bottom row is white.
    let json = r##"{
      "name": "demo",
      "tile-size": 16,
      "nodes": {
        "out": {
          "op": "gradient-linear",
          "start": [0, 0], "end": [0, 1],
          "stops": [[0, "#000000"], [1, "#ffffff"]]
        }
      },
      "output": "@out"
    }"##;
    let r = render(json, 16, 0);
    assert!(
        r.pixel(8, 0)[0] < 16,
        "top row should be black: {:?}",
        r.pixel(8, 0)
    );
    assert!(
        r.pixel(8, 15)[0] > 240,
        "bottom row should be white: {:?}",
        r.pixel(8, 15)
    );
    // Middle row should be roughly grey.
    let mid = r.pixel(8, 8)[0];
    assert!(
        (mid as i32 - 128).abs() < 16,
        "mid should be grey, got {mid}"
    );
}

#[test]
fn gradient_radial_center_to_edge() {
    let json = r##"{
      "name": "demo",
      "tile-size": 16,
      "nodes": {
        "out": {
          "op": "gradient-radial",
          "center": [0.5, 0.5], "radius": 0.5,
          "stops": [[0, "#ffffff"], [1, "#000000"]]
        }
      },
      "output": "@out"
    }"##;
    let r = render(json, 16, 0);
    let center = r.pixel(8, 8)[0];
    let corner = r.pixel(0, 0)[0];
    // Pixel sample center is offset by 0.5 from `[0.5, 0.5]`, so the
    // closest pixel is slightly off-center but still mostly white.
    assert!(center > 220, "center should be near white: {center}");
    assert!(
        corner < 32,
        "corner past radius should be near black: {corner}"
    );
}

#[test]
fn gradient_conic_sweeps_around_center() {
    // A full red→green→blue→red sweep. At 0° (right of center), red;
    // at 180° (left of center), should reach mid-stop colors.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "out": {
          "op": "gradient-conic",
          "center": [0.5, 0.5],
          "stops": [[0, "#ff0000"], [0.333, "#00ff00"], [0.667, "#0000ff"], [1, "#ff0000"]]
        }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    // Pixel to the right of center (ang ~ 0): red dominant.
    let right = r.pixel(24, 16);
    assert!(
        right[0] > 200 && right[1] < 64 && right[2] < 64,
        "right should be red: {right:?}"
    );
}

#[test]
fn gradient_diamond_has_axis_aligned_corners() {
    let json = r##"{
      "name": "demo",
      "tile-size": 16,
      "nodes": {
        "out": {
          "op": "gradient-diamond",
          "center": [0.5, 0.5], "radius": 0.5,
          "stops": [[0, "#ffffff"], [1, "#000000"]]
        }
      },
      "output": "@out"
    }"##;
    let r = render(json, 16, 0);
    // Diamond: the four cardinal-direction tips (at distance 0.5 along
    // an axis) reach t=1 (black). The corner (0.5+0.5=1.0 Manhattan
    // distance from center / 0.5 radius = 2.0 → clamped to 1 → black).
    assert!(r.pixel(8, 8)[0] > 200, "center should be near white");
    assert!(r.pixel(0, 0)[0] < 32, "corner should be near black");
    // A pixel halfway to the tip along an axis: Manhattan ~0.25, t ~0.5 → grey.
    let half = r.pixel(8, 4)[0];
    assert!(
        (half as i32 - 128).abs() < 40,
        "axial half should be grey, got {half}"
    );
}

/// `interpolation: step` paints flat bands, each the colour of the stop at
/// or below the position, as `color-ramp`'s `step` does; and a `string`
/// param can switch it per render.
#[test]
fn gradient_step_paints_the_stop_at_or_below() {
    let json = |op: &str, interpolation: &str| {
        format!(
            r##"{{
              "name": "step",
              "tile-size": 16,
              "params": {{ "i": {{ "type": "string", "enum": ["linear", "step"], "default": "linear" }} }},
              "nodes": {{
                "out": {{ "op": "{op}", "start": [0, 0], "end": [1, 0],
                         "center": [0.5, 0.5], "radius": 0.6,
                         "stops": [[0.25, "#ff0000"], [0.5, "#00ff00"], [0.75, "#0000ff"]],
                         "interpolation": {interpolation} }}
              }},
              "output": "@out"
            }}"##
        )
    };
    const STOPS: [[u8; 4]; 3] = [[0xff, 0, 0, 0xff], [0, 0xff, 0, 0xff], [0, 0, 0xff, 0xff]];
    let linear_step = render(&json("gradient-linear", r#""step""#), 16, 0);
    // A linear gradient's positions run (x + 0.5) / 16 across.
    for x in 0..16 {
        let t = (x as f32 + 0.5) / 16.0;
        // Below the first stop, its colour too.
        let want = STOPS[if t < 0.5 {
            0
        } else if t < 0.75 {
            1
        } else {
            2
        }];
        assert_eq!(linear_step.pixel(x, 8), want, "x {x}");
    }
    for op in [
        "gradient-linear",
        "gradient-radial",
        "gradient-conic",
        "gradient-diamond",
    ] {
        let r = render(&json(op, r#""step""#), 16, 0);
        // Nothing is blended: every pixel is one stop's colour.
        assert!(
            r.pixels.chunks(4).all(|p| STOPS.iter().any(|s| s == p)),
            "{op}"
        );
        // Linear blends between the stops instead.
        let linear = render(&json(op, r#""linear""#), 16, 0);
        assert_ne!(linear.pixels, r.pixels, "{op}");
        // The param form follows the override.
        let by_param = crate::common::render_with_params(
            &json(op, r#""$i""#),
            16,
            0,
            ezu_graph::TileId { z: 0, x: 0, y: 0 },
            &[("i", ezu_graph::ScalarValue::String("step".into()))],
        );
        assert_eq!(by_param.pixels, r.pixels, "{op}");
        assert_eq!(
            render(&json(op, r#""$i""#), 16, 0).pixels,
            linear.pixels,
            "{op}"
        );
    }
}
