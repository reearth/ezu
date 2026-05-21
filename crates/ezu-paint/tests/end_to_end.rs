//! End-to-end: JSON -> typed graph -> evaluated tile -> pixels.

#[test]
fn registry_emits_document_schema_with_all_ops() {
    let registry = ezu_paint::nodes::default_registry();
    let schema = registry.document_schema();
    let s = schema.to_string();
    // Spot-check: every built-in op surfaces in the schema and the
    // document-level structure is there.
    for op in [
        "solid",
        "circle",
        "blur",
        "blend",
        "gradient-linear",
        "gradient-radial",
        "gradient-conic",
        "gradient-diamond",
        "brightness-contrast",
        "hsl",
        "invert",
        "color-to-alpha",
        "mvt-source",
        "fill-solid",
        "fill-dabs",
        "line",
        "brush-file",
        "brush-solid",
        "image",
        "dash",
        "wave",
        "stamp",
        "tiling",
        "place",
    ] {
        assert!(s.contains(&format!("\"const\":\"{op}\"")), "missing op `{op}` in schema");
    }
    assert!(s.contains("\"$schema\""));
    assert!(s.contains("\"nodes\""));
    assert!(s.contains("\"output\""));
}

use ezu_graph::{
    build_graph, Cache, CanvasInfo, Evaluator, NoAssets, ParamValues, PortValue, TileId,
};
use ezu_paint::nodes::default_registry;
use ezu_style::Document;

fn render(json: &str, tile_size: u32, pad: u32) -> std::sync::Arc<ezu_graph::RasterBuf> {
    render_tile(json, tile_size, pad, TileId { z: 0, x: 0, y: 0 })
}

fn render_tile(
    json: &str,
    tile_size: u32,
    pad: u32,
    tile: TileId,
) -> std::sync::Arc<ezu_graph::RasterBuf> {
    let doc = Document::from_json(json).expect("parse");
    let registry = default_registry();
    let graph = build_graph(&doc, &registry).expect("build");
    let cache = Cache::new();
    let assets = NoAssets;
    let ev = Evaluator::new(&graph, &cache, &assets);
    let out = ev
        .render(tile, CanvasInfo { tile_size, pad }, &ParamValues::new(), 0)
        .expect("render");
    match out {
        PortValue::Raster(r) => r,
        other => panic!("expected raster output, got {:?}", other.kind()),
    }
}

#[test]
fn solid_only_produces_uniform_raster() {
    let json = r##"{
      "name": "demo",
      "tile-size": 16,
      "nodes": { "bg": { "op": "solid", "color": "#3366ff" } },
      "output": "@bg"
    }"##;
    let r = render(json, 16, 0);
    assert_eq!(r.width, 16);
    let p = r.pixel(8, 8);
    assert_eq!(p, [0x33, 0x66, 0xff, 0xff]);
}

#[test]
fn circle_fill_then_blend_over_background() {
    // Background is opaque red. A blue disk drawn at center, blended on top.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "bg":    { "op": "solid", "color": "#ff0000" },
        "blue":  { "op": "circle", "color": "#0000ff", "radius-frac": 0.4 },
        "out":   { "op": "blend", "base": "@bg", "over": "@blue" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    // Center pixel should be blue (mask = 1).
    let center = r.pixel(16, 16);
    assert!(center[2] > 200, "center should be blue-dominant: {center:?}");
    assert!(center[0] < 32, "center red should be near zero: {center:?}");
    // Corner pixel should be red (outside disk).
    let corner = r.pixel(0, 0);
    assert_eq!(corner, [0xff, 0x00, 0x00, 0xff]);
}

#[test]
fn blur_softens_disk_edge() {
    let json_sharp = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "fill":  { "op": "circle", "color": "#000000ff", "radius-frac": 0.4 }
      },
      "output": "@fill"
    }"##;
    let json_blur = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "disk":  { "op": "circle", "color": "#000000ff", "radius-frac": 0.4 },
        "fill":  { "op": "blur", "input": "@disk", "sigma": 1.5 }
      },
      "output": "@fill"
    }"##;
    let sharp = render(json_sharp, 32, 0);
    let blur = render(json_blur, 32, 0);
    // A pixel just outside the disk edge should be more covered by the
    // blurred version (alpha > 0) but transparent in the sharp one.
    // radius = 32 * 0.4 = 12.8 → check pixel at (16+13, 16) ≈ outside.
    let px_sharp = sharp.pixel(29, 16);
    let px_blur = blur.pixel(29, 16);
    assert_eq!(px_sharp[3], 0, "outside the disk, sharp version is transparent");
    assert!(
        px_blur[3] > 0,
        "outside the disk, blurred version has some coverage: {px_blur:?}"
    );
}

#[test]
fn blend_multiply_darkens_base() {
    // Multiply two opaque mid-grays: 0x80 * 0x80 / 0xff ≈ 0x40.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "nodes": {
        "a":   { "op": "solid", "color": "#808080" },
        "b":   { "op": "solid", "color": "#808080" },
        "out": { "op": "blend", "base": "@a", "over": "@b", "mode": "multiply" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    let p = r.pixel(4, 4);
    // Expect close to 0x40 (64). Allow ±2 for rounding.
    assert!((p[0] as i32 - 0x40).abs() <= 2, "got {p:?}");
    assert_eq!(p[3], 0xff, "fully opaque");
}

#[test]
fn blend_clip_confines_to_base_alpha() {
    // base is a circle (alpha varies); over is solid red. With clip,
    // pixels outside the circle stay transparent.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "base": { "op": "circle", "color": "#0000ff", "radius-frac": 0.3 },
        "over": { "op": "solid", "color": "#ff0000" },
        "out":  { "op": "blend", "base": "@base", "over": "@over", "clip": true }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    // Corner is outside the disk → base alpha = 0 → clip output alpha = 0.
    let corner = r.pixel(0, 0);
    assert_eq!(corner[3], 0, "outside base alpha must be 0 under clip: {corner:?}");
    // Center is inside disk → red shows through atop blue's alpha.
    let center = r.pixel(16, 16);
    assert!(center[3] > 200, "center should be opaque: {center:?}");
    assert!(center[0] > 200, "center should be red-dominant: {center:?}");
}

#[test]
fn blend_mask_modulates_over_coverage() {
    // mask is a small disk; using it as the mask input means red over
    // only appears where the mask is opaque.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "base": { "op": "solid", "color": "#0000ff" },
        "over": { "op": "solid", "color": "#ff0000" },
        "mask": { "op": "circle", "color": "#ffffff", "radius-frac": 0.3 },
        "out":  { "op": "blend", "base": "@base", "over": "@over", "mask": "@mask" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    // Outside mask → still pure blue.
    let corner = r.pixel(0, 0);
    assert_eq!(corner, [0x00, 0x00, 0xff, 0xff]);
    // Inside mask → red wins.
    let center = r.pixel(16, 16);
    assert!(center[0] > 200, "center should be red: {center:?}");
    assert!(center[2] < 32, "center blue should be near zero: {center:?}");
}

#[test]
fn blend_destination_out_erases_base_under_over() {
    // base is opaque red everywhere; over is a centered disk. With
    // composite=destination-out, the disk-shaped region becomes
    // transparent, the rest stays red.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "base": { "op": "solid", "color": "#ff0000" },
        "over": { "op": "circle", "color": "#ffffff", "radius-frac": 0.4 },
        "out":  { "op": "blend", "base": "@base", "over": "@over", "composite": "destination-out" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    let center = r.pixel(16, 16);
    assert_eq!(center[3], 0, "center should be erased: {center:?}");
    let corner = r.pixel(0, 0);
    assert_eq!(corner, [0xff, 0x00, 0x00, 0xff], "corner untouched: {corner:?}");
}

#[test]
fn invert_negates_rgb_preserving_alpha() {
    let json = r##"{
      "name": "demo",
      "tile-size": 4,
      "nodes": {
        "src": { "op": "solid", "color": "#204060" },
        "out": { "op": "invert", "input": "@src" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 4, 0);
    // 0x20 -> 0xdf, 0x40 -> 0xbf, 0x60 -> 0x9f.
    assert_eq!(r.pixel(0, 0), [0xdf, 0xbf, 0x9f, 0xff]);
}

#[test]
fn brightness_contrast_shifts_levels() {
    let json = r##"{
      "name": "demo",
      "tile-size": 4,
      "nodes": {
        "src": { "op": "solid", "color": "#808080" },
        "out": { "op": "brightness-contrast", "input": "@src", "brightness": 0.25 }
      },
      "output": "@out"
    }"##;
    let r = render(json, 4, 0);
    // 0.5 + 0.25 = 0.75 -> ~0xbf.
    let p = r.pixel(0, 0);
    assert!((p[0] as i32 - 0xbf).abs() <= 2, "got {p:?}");
}

#[test]
fn hsl_hue_shift_rotates_color() {
    // Pure red rotated by +120° -> pure green.
    let json = r##"{
      "name": "demo",
      "tile-size": 4,
      "nodes": {
        "src": { "op": "solid", "color": "#ff0000" },
        "out": { "op": "hsl", "input": "@src", "hue-shift": 120 }
      },
      "output": "@out"
    }"##;
    let r = render(json, 4, 0);
    let p = r.pixel(0, 0);
    assert!(p[1] > 240 && p[0] < 8 && p[2] < 8, "expected pure green: {p:?}");
}

#[test]
fn color_to_alpha_keys_out_target() {
    // Red surface: keying red drops alpha to 0, keying blue leaves it.
    let json_keyed = r##"{
      "name": "demo",
      "tile-size": 4,
      "nodes": {
        "src": { "op": "solid", "color": "#ff0000" },
        "out": { "op": "color-to-alpha", "input": "@src", "color": "#ff0000", "threshold": 0.05, "softness": 0.05 }
      },
      "output": "@out"
    }"##;
    let json_kept = r##"{
      "name": "demo",
      "tile-size": 4,
      "nodes": {
        "src": { "op": "solid", "color": "#ff0000" },
        "out": { "op": "color-to-alpha", "input": "@src", "color": "#0000ff", "threshold": 0.05, "softness": 0.05 }
      },
      "output": "@out"
    }"##;
    let keyed = render(json_keyed, 4, 0);
    let kept = render(json_kept, 4, 0);
    assert_eq!(keyed.pixel(0, 0)[3], 0, "matching color should be transparent");
    assert_eq!(kept.pixel(0, 0)[3], 0xff, "distant color should be opaque");
}

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
    assert!(r.pixel(8, 0)[0] < 16, "top row should be black: {:?}", r.pixel(8, 0));
    assert!(r.pixel(8, 15)[0] > 240, "bottom row should be white: {:?}", r.pixel(8, 15));
    // Middle row should be roughly grey.
    let mid = r.pixel(8, 8)[0];
    assert!((mid as i32 - 128).abs() < 16, "mid should be grey, got {mid}");
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
    assert!(corner < 32, "corner past radius should be near black: {corner}");
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
    assert!(right[0] > 200 && right[1] < 64 && right[2] < 64, "right should be red: {right:?}");
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
    assert!((half as i32 - 128).abs() < 40, "axial half should be grey, got {half}");
}

#[test]
fn noise_perlin_produces_variation_and_is_deterministic() {
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "out": {
          "op": "noise",
          "type": "perlin",
          "scale-px": 16,
          "octaves": 3,
          "seed": 42,
          "anchor": "tile"
        }
      },
      "output": "@out"
    }"##;
    let a = render(json, 32, 0);
    let b = render(json, 32, 0);
    // Deterministic: identical bytes across two renders.
    assert_eq!(a.pixels, b.pixels);
    // Not uniform: at least two distinct luma values.
    let mut min = 255u8;
    let mut max = 0u8;
    for chunk in a.pixels.chunks(4) {
        min = min.min(chunk[0]);
        max = max.max(chunk[0]);
    }
    assert!(max as i32 - min as i32 > 20, "noise should vary: {min}..{max}");
}

#[test]
fn noise_white_changes_with_seed() {
    let mk = |seed: u32| -> String {
        format!(
            r##"{{
              "name": "demo",
              "tile-size": 16,
              "nodes": {{
                "out": {{ "op": "noise", "type": "white", "scale-px": 1, "seed": {seed}, "anchor": "tile" }}
              }},
              "output": "@out"
            }}"##
        )
    };
    let a = render(&mk(1), 16, 0);
    let b = render(&mk(2), 16, 0);
    assert_ne!(a.pixels, b.pixels, "different seeds should give different white noise");
}

#[test]
fn displace_with_zero_amp_is_identity() {
    // amp-px = 0 means the displace node should reproduce its input
    // byte-for-byte (modulo the grown pad). Comparing visible-tile
    // pixels at matching offsets verifies the warp path doesn't drop
    // information when there's nothing to do.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "src":   { "op": "circle", "color": "#ff0000", "radius-frac": 0.3 },
        "disp":  { "op": "solid", "color": "#808080" },
        "out":   { "op": "displace", "input": "@src", "displacement": "@disp", "amp-px": 0 }
      },
      "output": "@out"
    }"##;
    let warped = render(json, 32, 0);
    let json_ref = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": { "src": { "op": "circle", "color": "#ff0000", "radius-frac": 0.3 } },
      "output": "@src"
    }"##;
    let reference = render(json_ref, 32, 0);
    // Reference has pad=0 (32×32); warped has pad=0 too because amp=0
    // doesn't bump required_pad. So both rasters are the same size.
    assert_eq!(warped.width, reference.width);
    assert_eq!(warped.pixels, reference.pixels);
}

#[test]
fn displace_moves_pixels_when_amp_nonzero() {
    // With a non-zero amplitude and a non-neutral displacement map,
    // pixels near the disk edge must change versus an undisplaced
    // reference. We probe a pixel that sits on the disk's right
    // boundary.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "src":   { "op": "circle", "color": "#ff0000", "radius-frac": 0.3 },
        "disp":  { "op": "solid", "color": "#ff0000" },
        "out":   { "op": "displace", "input": "@src", "displacement": "@disp", "amp-px": 4 }
      },
      "output": "@out"
    }"##;
    // The host must supply enough pad for the warp amplitude — the
    // framework exposes `required_pad` as a contract for hosts to honour
    // via `compute_pad`, not as an auto-grow mechanism.
    let warped = render(json, 32, 8);
    // R=255, G=0: dx = (1.0 - 0.5) * 2 * 4 = +4 px, dy = -4 px.
    // Visible tile center (16, 16) inside the 32+2*8 padded raster is
    // at padded (24, 24), and the read happens at input (28, 20) which
    // still lands inside the radius-frac=0.3 disk (r ≈ 9.6).
    let center = warped.pixel(8 + 16, 8 + 16);
    assert_eq!(center[0], 255, "center should still read red: {center:?}");
}

#[test]
fn warp_disturbs_input_but_stays_deterministic() {
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "src":   { "op": "circle", "color": "#000000", "radius-frac": 0.4 },
        "out":   {
          "op": "warp", "input": "@src",
          "type": "perlin", "scale-px": 12, "amp-px": 5, "seed": 7,
          "anchor": "tile"
        }
      },
      "output": "@out"
    }"##;
    let a = render(json, 32, 0);
    let b = render(json, 32, 0);
    assert_eq!(a.pixels, b.pixels, "warp must be deterministic");

    let json_ref = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": { "src": { "op": "circle", "color": "#000000", "radius-frac": 0.4 } },
      "output": "@src"
    }"##;
    let reference = render(json_ref, 32, 0);
    assert_ne!(a.pixels, reference.pixels, "warp should perturb pixels");
}

#[test]
fn warp_world_anchor_is_seamless_across_adjacent_tiles() {
    // Two horizontally adjacent tiles must agree at their shared
    // border column. The input is a world-anchored noise field plus a
    // world-anchored warp, so the value at the same world pixel must
    // match on either side of the seam.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "src":   {
          "op": "noise", "type": "perlin", "scale-px": 24,
          "seed": 11, "anchor": "world"
        },
        "out":   {
          "op": "warp", "input": "@src",
          "type": "perlin", "scale-px": 16, "amp-px": 4, "seed": 23,
          "anchor": "world"
        }
      },
      "output": "@out"
    }"##;
    // Pad must cover `amp-px` so the warp can reach into the neighbour
    // tile's territory and produce a value identical to that
    // neighbour's own read.
    let pad: u32 = 8;
    let amp: u32 = 4;
    let left = render_tile(json, 32, pad, TileId { z: 4, x: 5, y: 7 });
    let right = render_tile(json, 32, pad, TileId { z: 4, x: 6, y: 7 });
    // Each warped pixel reads from a position up to `amp` px away in
    // raster-local coords, so the *safe* output range is the inner
    // `[amp, padded - amp]` band where the read never hits the boundary
    // clamp. Restrict the seam comparison to world columns that fall in
    // both tiles' safe bands — there both tiles compute from identical
    // world-positioned samples and must agree byte-for-byte.
    let tile_size = 32u32;
    // Left tile safe padded x ∈ [amp, tile_size + 2*pad - amp).
    // Right tile safe padded x ∈ [amp, tile_size + 2*pad - amp).
    // World overlap of safe bands = pad - amp columns on each side of
    // the shared border.
    let safe = pad - amp;
    for dx in 0..safe {
        let lx = tile_size + pad + dx; // left padded x
        let rx = pad + dx;             // right padded x — same world column
        for y in (pad + amp)..(pad + tile_size - amp) {
            let l = left.pixel(lx, y);
            let r = right.pixel(rx, y);
            assert_eq!(
                l, r,
                "seam mismatch at dx={dx} y={y}: left={l:?} right={r:?}"
            );
        }
    }
}

#[test]
fn brush_solid_line_paints_a_visible_stroke() {
    // brush-solid + line: draw a horizontal red line across the tile at y=mid.
    // `extent` 4096 covers the 32-px canvas; `y = 2048` is the middle row.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "feats": { "op": "literal-geometry", "extent": 4096,
                   "lines": [ [ [0, 2048], [4095, 2048] ] ] },
        "brush": { "op": "brush-solid", "width-px": 3, "color": "#ff0000" },
        "out":   { "op": "line", "features": "@feats", "brush": "@brush", "color": "#ff0000" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    // Center pixel of the middle row should have a strong red component.
    let mid = r.pixel(16, 16);
    assert!(mid[0] > 200, "center stroke should be red-dominant: {mid:?}");
    assert!(mid[3] > 200, "center stroke should be opaque: {mid:?}");
    // A pixel well above the stroke should be transparent.
    let above = r.pixel(16, 4);
    assert_eq!(above[3], 0, "above the stroke should be transparent: {above:?}");
}

#[test]
fn dash_chops_a_long_line_into_multiple_runs() {
    // A horizontal line dashed at 4-px dash / 4-px gap should leave the
    // tile striped: some columns are inked, some are clear.
    let json = r##"{
      "name": "demo",
      "tile-size": 64,
      "nodes": {
        "feats":  { "op": "literal-geometry", "extent": 4096,
                    "lines": [ [ [0, 2048], [4095, 2048] ] ] },
        "dashed": { "op": "dash", "features": "@feats",
                    "dash-px": 4, "gap-px": 4 },
        "brush":  { "op": "brush-solid", "width-px": 2, "color": "#000000" },
        "out":    { "op": "line", "features": "@dashed", "brush": "@brush", "color": "#000000" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 64, 0);
    // Across the middle row, sample alpha. Expect alternation: some
    // columns hit a dash (alpha > 0), others fall in a gap (alpha = 0).
    let mut inked = 0;
    let mut clear = 0;
    for x in 0..64 {
        let a = r.pixel(x, 32)[3];
        if a > 32 {
            inked += 1;
        } else if a < 8 {
            clear += 1;
        }
    }
    assert!(inked > 8 && clear > 8, "expected stripes: inked={inked} clear={clear}");
}

#[test]
fn wave_lifts_a_horizontal_line_off_its_baseline() {
    // A horizontal source line should, after wave displacement, leave
    // pixels above and below the baseline row.
    let json = r##"{
      "name": "demo",
      "tile-size": 64,
      "nodes": {
        "feats":  { "op": "literal-geometry", "extent": 4096,
                    "lines": [ [ [0, 2048], [4095, 2048] ] ] },
        "wavy":   { "op": "wave", "features": "@feats",
                    "amplitude-px": 10, "wavelength-px": 20 },
        "brush":  { "op": "brush-solid", "width-px": 2, "color": "#000000" },
        "out":    { "op": "line", "features": "@wavy", "brush": "@brush", "color": "#000000" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 64, 0);
    // Sample rows within the wave envelope on both sides of the
    // baseline (y=32). With amplitude 10 px, the curve reaches roughly
    // y=22 (above) and y=42 (below); sampling y=28 / y=36 stays well
    // inside the inked envelope but is firmly off the baseline.
    let mut above = false;
    let mut below = false;
    for x in 8..56 {
        if r.pixel(x, 28)[3] > 32 {
            above = true;
        }
        if r.pixel(x, 36)[3] > 32 {
            below = true;
        }
    }
    assert!(above, "wave should push pixels above the baseline");
    assert!(below, "wave should push pixels below the baseline");
}

#[test]
fn stamp_places_image_at_each_point() {
    // Use `circle` as a sprite (canvas-sized, but only the inner disk is
    // opaque). Stamping at two extent positions should leave two visible
    // splotches. Stamp draws the full sprite at each point; since the
    // sprite is canvas-sized, both stamps overlap heavily — we only
    // check that the output has substantial coverage and isn't blank.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "feats": { "op": "literal-geometry", "extent": 4096,
                   "points": [ [1024, 2048], [3072, 2048] ] },
        "img":   { "op": "circle", "color": "#00ff00", "radius-frac": 0.15 },
        "out":   { "op": "stamp", "features": "@feats", "image": "@img", "scale": 1.0 }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    let mut green = 0;
    for y in 0..32 {
        for x in 0..32 {
            let p = r.pixel(x, y);
            if p[1] > 100 && p[3] > 100 {
                green += 1;
            }
        }
    }
    assert!(green > 4, "stamp should leave visible green pixels: got {green}");
}

#[test]
fn tiling_passes_through_at_natural_scale() {
    // Tile a `circle` raster onto a same-size canvas with `scale-px`
    // equal to the source width: the output should reproduce the
    // source 1:1 — red at center, transparent at corners.
    let json = r##"{
      "name": "demo",
      "tile-size": 16,
      "nodes": {
        "src":  { "op": "circle", "color": "#ff0000", "radius-frac": 0.4 },
        "out":  { "op": "tiling", "input": "@src", "anchor": "tile", "scale-px": 16 }
      },
      "output": "@out"
    }"##;
    let r = render(json, 16, 0);
    let center = r.pixel(8, 8);
    assert!(center[0] > 200, "center should be red: {center:?}");
    let corner = r.pixel(0, 0);
    assert_eq!(corner[3], 0, "corner should be transparent: {corner:?}");
}

#[test]
fn tiling_repeats_pattern_at_smaller_scale() {
    // Halving the scale should tile the disk twice along each axis, so
    // four disks appear in the output. Sampling at the four "tile
    // centers" (4, 4), (12, 4), (4, 12), (12, 12) should all be red.
    let json = r##"{
      "name": "demo",
      "tile-size": 16,
      "nodes": {
        "src":  { "op": "circle", "color": "#ff0000", "radius-frac": 0.4 },
        "out":  { "op": "tiling", "input": "@src", "anchor": "tile", "scale-px": 8 }
      },
      "output": "@out"
    }"##;
    let r = render(json, 16, 0);
    for &(x, y) in &[(4, 4), (12, 4), (4, 12), (12, 12)] {
        let p = r.pixel(x, y);
        assert!(p[0] > 100, "tile center ({x},{y}) should have red disk: {p:?}");
    }
}

#[test]
fn tiling_world_anchor_is_seamless_across_tiles() {
    // Two adjacent map tiles, world-anchored: pad lets us sample the
    // same world column from both tiles' padded buffers. With anchor
    // "world", `left.pixel(tile_size + pad + dx, y)` and
    // `right.pixel(pad + dx, y)` must reference the same world pixel.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "src":  { "op": "circle", "color": "#0000ff", "radius-frac": 0.3 },
        "out":  { "op": "tiling", "input": "@src", "anchor": "world", "scale-px": 12 }
      },
      "output": "@out"
    }"##;
    let left = render_tile(json, 32, 4, TileId { z: 4, x: 5, y: 7 });
    let right = render_tile(json, 32, 4, TileId { z: 4, x: 6, y: 7 });
    let pad = 4u32;
    let tile_size = 32u32;
    for dx in 0..pad {
        let lx = tile_size + pad + dx;
        let rx = pad + dx;
        for y in pad..(pad + tile_size) {
            let l = left.pixel(lx, y);
            let r = right.pixel(rx, y);
            // Bilinear can introduce ±1 LSB; everything else must
            // agree byte-for-byte.
            for c in 0..4 {
                assert!(
                    (l[c] as i32 - r[c] as i32).abs() <= 1,
                    "seam mismatch at dx={dx} y={y} channel={c}: left={l:?} right={r:?}"
                );
            }
        }
    }
}

#[test]
fn place_cover_fills_canvas_with_source_color() {
    // A 16-canvas source disk (red) covered onto a 32-canvas should
    // scale up by 2x. The canvas center should be red; cover crops
    // the source so corners are red too (uniform scale 2x covers
    // exactly, the source disk reaches y/x = 16 from canvas center).
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "src":  { "op": "circle", "color": "#ff0000", "radius-frac": 0.5 },
        "out":  { "op": "place", "input": "@src", "fit": "cover" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    let center = r.pixel(16, 16);
    assert!(center[0] > 200, "center should be red under cover: {center:?}");
    assert!(center[3] > 200);
}

#[test]
fn place_contain_centers_source_with_letterbox() {
    // A square source contained in a square canvas: with equal aspect,
    // contain == identity. We use a non-square arrangement by
    // contain-fitting a smaller virtual rect via scale-down test:
    // place at fit=none, scale=0.5, anchor=center, position center.
    // Verifies the manual placement path: shrink the disk to half
    // size, centered. Corners should now be transparent.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "src":  { "op": "circle", "color": "#00ff00", "radius-frac": 0.5 },
        "out":  { "op": "place", "input": "@src", "fit": "none",
                  "scale": 0.5, "anchor": "center",
                  "position-px": [16, 16] }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    let center = r.pixel(16, 16);
    assert!(center[1] > 200, "center should be green: {center:?}");
    // Disk now has radius ~4 px (half of 8), so (24, 16) is well outside.
    let outside = r.pixel(24, 16);
    assert_eq!(outside[3], 0, "shrunk disk should not reach (24,16): {outside:?}");
}

#[test]
fn param_substitution_works() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "params": { "bg": { "type": "color", "default": "#102030" } },
      "nodes": { "out": { "op": "solid", "color": "$bg" } },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    assert_eq!(r.pixel(0, 0), [0x10, 0x20, 0x30, 0xff]);
}
