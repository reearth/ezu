//! `blur` over a `ScalarField` — generalising an elevation field before
//! the terrain ops read it.

use crate::common::render_with_scalar_fields;
use ezu_graph::{build_graph, GeoScale, PortKind, ScalarField, TileId};
use ezu_paint::nodes::default_registry;
use ezu_style::Document;

const TILE: TileId = TileId { z: 0, x: 0, y: 0 };
const SIZE: u32 = 32;
/// Covers blur's `ceil(3σ)` reach at σ = 2 plus hillshade's one pixel.
const PAD: u32 = 8;

/// A padded-canvas field of `f(x, y)` with the given geographic scale.
fn field(geo_scale: Option<GeoScale>, f: impl Fn(u32, u32) -> f32) -> ScalarField {
    let n = SIZE + 2 * PAD;
    let mut values = Vec::with_capacity((n * n) as usize);
    for y in 0..n {
        for x in 0..n {
            values.push(f(x, y));
        }
    }
    ScalarField {
        width: n,
        height: n,
        values: values.into(),
        nodata: None,
        geo_scale,
    }
}

/// `dem → [blur →] hillshade`; `blur` is spliced in when `sigma` is set.
fn doc(sigma: Option<f64>) -> String {
    let (blur, shade_in) = match sigma {
        Some(s) => (
            format!(r#""blurred": {{ "op": "blur", "input": "@dem", "sigma": {s} }},"#),
            "@blurred",
        ),
        None => (String::new(), "@dem"),
    };
    format!(
        r##"{{
          "name": "blur-field-test",
          "tile-size": {SIZE},
          "pad": {PAD},
          "sources": {{
            "terrain": {{ "type": "dem",
                          "url": "http://example.invalid/{{z}}/{{x}}/{{y}}.webp",
                          "encoding": "terrarium" }}
          }},
          "nodes": {{
            "dem": {{ "op": "dem" }},
            {blur}
            "out": {{ "op": "hillshade", "field": "{shade_in}" }}
          }},
          "output": "@out"
        }}"##
    )
}

#[test]
fn blur_over_a_field_is_a_field_and_feeds_hillshade() {
    let style = Document::from_json(&doc(Some(2.0))).expect("parse");
    let graph = build_graph(&style, &default_registry()).expect("build");
    let blurred = graph.index_of("blurred").expect("blurred node");
    assert_eq!(graph.output_kind(blurred), PortKind::ScalarField);

    // A bumpy ridge renders through, and the blur visibly smooths it.
    let bumpy = field(None, |x, y| {
        (x as f32 * 0.5).min(30.0 - x as f32 * 0.5) * 4.0 + ((x * 7 + y * 13) % 5) as f32
    });
    let sharp =
        render_with_scalar_fields(&doc(None), SIZE, PAD, TILE, &[("terrain", bumpy.clone())]);
    let smooth = render_with_scalar_fields(&doc(Some(2.0)), SIZE, PAD, TILE, &[("terrain", bumpy)]);
    assert_eq!((smooth.width, smooth.height), (sharp.width, sharp.height));
    assert_ne!(smooth.pixels, sharp.pixels);
}

#[test]
fn blur_keeps_the_fields_geo_scale() {
    // A planar ramp is its own blur, so with the geographic scale kept,
    // shading the blurred ramp matches shading the ramp. Dropping the
    // scale would read the 5 m/px rise as 50 m/px and shade far darker.
    let scale = Some(GeoScale {
        metres_per_pixel_x: 10.0,
        metres_per_pixel_y: 10.0,
    });
    let ramp = |gs| field(gs, |x, y| x as f32 * 5.0 + y as f32 * 2.0);
    let plain = render_with_scalar_fields(&doc(None), SIZE, PAD, TILE, &[("terrain", ramp(scale))]);
    let blurred = render_with_scalar_fields(
        &doc(Some(2.0)),
        SIZE,
        PAD,
        TILE,
        &[("terrain", ramp(scale))],
    );
    let unscaled =
        render_with_scalar_fields(&doc(None), SIZE, PAD, TILE, &[("terrain", ramp(None))]);
    // The render is the padded canvas; compare the tile inside it, where
    // neither op reaches the clamped edge.
    for y in PAD..PAD + SIZE {
        for x in PAD..PAD + SIZE {
            assert_eq!(plain.pixel(x, y), blurred.pixel(x, y), "({x}, {y})");
        }
    }
    assert_ne!(
        plain.pixel(SIZE / 2, SIZE / 2),
        unscaled.pixel(SIZE / 2, SIZE / 2)
    );
}
