//! A symbol's icon and text are placed as one unit against the shared
//! collision index: both boxes must be free or the whole symbol drops, the
//! `*-optional` flags admit the half that fits, and an icon blocks other
//! layers' labels the way its text does.

mod common;
use common::render_with_features_and_sprite;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::{RasterBuf, SpriteRect, SpriteSheet, TileId};
use std::collections::HashMap;

/// Tile geometry the tests reason in: `EXTENT / TILE` extent units per px.
const TILE: u32 = 128;
const PAD: u32 = 16;
const EXTENT: i32 = 4096;

/// Extent coords of a tile-pixel position.
fn at(px: f32, py: f32) -> (i32, i32) {
    let u = EXTENT as f32 / TILE as f32;
    ((px * u) as i32, (py * u) as i32)
}

fn font_url() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
    format!("file:{}", path.display()).replace('\\', "/")
}

/// A point feature at tile-pixel coords carrying its label in `name`.
fn point(name: &str, px: f32, py: f32) -> Feature {
    let (x, y) = at(px, py);
    let mut properties = HashMap::new();
    properties.insert("name".to_string(), Value::String(name.to_string()));
    let mut geometry = Geometry::default();
    geometry.points.push((x, y));
    Feature {
        id: None,
        geometry,
        properties,
    }
}

fn layer(name: &str, features: Vec<Feature>) -> FeatureLayer {
    FeatureLayer {
        name: name.to_string(),
        extent: EXTENT as u32,
        features,
    }
}

/// A sheet with one 16×16 opaque green icon, `dot`.
fn dot_sheet() -> SpriteSheet {
    let mut atlas = RasterBuf::new(16, 16);
    for px in atlas.pixels.chunks_exact_mut(4) {
        px.copy_from_slice(&[0, 255, 0, 255]);
    }
    let mut icons = HashMap::new();
    icons.insert(
        "dot".to_string(),
        SpriteRect {
            x: 0,
            y: 0,
            width: 16,
            height: 16,
            pixel_ratio: 1.0,
            ..SpriteRect::default()
        },
    );
    SpriteSheet { atlas, icons }
}

/// Opaque pixels that read as the icon's green, and as label ink (any
/// non-green opaque pixel — the labels are drawn in red / blue).
fn counts(r: &RasterBuf) -> (usize, usize) {
    let (mut green, mut ink) = (0, 0);
    for y in 0..r.height {
        for x in 0..r.width {
            let p = r.pixel(x, y);
            if p[3] < 100 {
                continue;
            }
            if p[1] > p[0] && p[1] > p[2] {
                green += 1;
            } else {
                ink += 1;
            }
        }
    }
    (green, ink)
}

/// A two-label-layer recipe: `roads` (red) and `pois` (blue, the icon
/// carrier). `order` is the `label-placement` list, bottom first — the last
/// entry places first. `poi_fields` is the POI layer's whole field list
/// (label, icon, flags); `output` names the node to render.
fn recipe(order: &str, poi_fields: &str, output: &str) -> String {
    format!(
        r##"{{
      "name": "icon-labels",
      "tile-size": {TILE},
      "sources": {{
        "src":   {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body":  {{ "type": "font", "url": "{font}" }},
        "sheet": {{ "type": "sprite", "image": "builtin:atlas",
                    "index": {{ "dot": {{ "x": 0, "y": 0, "width": 16, "height": 16 }} }} }}
      }},
      "nodes": {{
        "road_feats": {{ "op": "features", "source": "src", "layer": "roads" }},
        "poi_feats":  {{ "op": "features", "source": "src", "layer": "pois" }},
        "road_labels": {{ "op": "text-labels", "features": "@road_feats", "font": ["body"],
                          "text": ["get", "name"], "size": 12, "color": "#ff0000",
                          "source": "src", "layer": "roads" }},
        "poi_labels":  {{ "op": "text-labels", "features": "@poi_feats",
                          "source": "src", "layer": "pois",
                          {poi_fields} }},
        "placed": {{ "op": "label-placement", "labels": [{order}] }},
        "roads": {{ "op": "text-draw", "labels": "@road_labels", "placement": "@placed" }},
        "pois":  {{ "op": "text-draw", "labels": "@poi_labels",  "placement": "@placed" }},
        "both":  {{ "op": "stack", "layers": ["@roads", "@pois"] }}
      }},
      "output": "@{output}"
    }}"##,
        font = font_url(),
    )
}

/// The POI layer's label: blue, 2 em (24 px) right of the point.
const TEXT: &str = r##""font": ["body"], "text": ["get", "name"], "size": 12,
                       "color": "#0000ff", "offset-em": [2, 0]"##;
/// The icon fields a symbol layer carries.
const ICON: &str = r#""icon-sprite": "@sheet", "icon-name": "dot""#;

/// `roads` on top (placing first, so a road label can knock a POI out).
const ROADS_FIRST: &str = r#""@poi_labels", "@road_labels""#;
/// `pois` on top (placing first, so a POI's boxes block road labels).
const POIS_FIRST: &str = r#""@road_labels", "@poi_labels""#;

fn render(
    recipe: &str,
    tile: TileId,
    roads: Vec<Feature>,
    pois: Vec<Feature>,
) -> std::sync::Arc<RasterBuf> {
    render_with_features_and_sprite(
        recipe,
        TILE,
        PAD,
        tile,
        &[
            ("src.roads", layer("roads", roads)),
            ("src.pois", layer("pois", pois)),
        ],
        "atlas",
        dot_sheet(),
    )
}

const T0: TileId = TileId { z: 0, x: 0, y: 0 };

/// The POI: its icon sits on the point, its label 2 em (24 px) to the right.
fn poi() -> Vec<Feature> {
    vec![point("II", 64.0, 64.0)]
}
/// A road label over the POI's text box, clear of its icon box.
fn over_text() -> Vec<Feature> {
    vec![point("MM", 90.0, 64.0)]
}
/// A road label over the POI's icon box, clear of its text box.
fn over_icon() -> Vec<Feature> {
    vec![point("I", 64.0, 64.0)]
}

#[test]
fn an_icon_and_its_text_place_or_drop_together() {
    // With both boxes free the symbol draws icon and label; a road label
    // taking the text's box drops the *whole* symbol, icon included — no
    // orphan icon survives its label.
    let r = recipe(ROADS_FIRST, &format!("{TEXT}, {ICON}"), "pois");
    let (green, ink) = counts(&render(&r, T0, vec![], poi()));
    assert!(
        green > 0 && ink > 0,
        "icon and label both draw: {green}/{ink}"
    );

    let (green, ink) = counts(&render(&r, T0, over_text(), poi()));
    assert_eq!(
        (green, ink),
        (0, 0),
        "a blocked text box must take its icon down with it"
    );
}

#[test]
fn text_optional_keeps_the_icon_when_the_label_is_blocked() {
    // MapLibre `text-optional`: the icon places on its own when only the
    // text's box is taken.
    let r = recipe(
        ROADS_FIRST,
        &format!("{TEXT}, {ICON}, \"text-optional\": true"),
        "pois",
    );
    let (green, ink) = counts(&render(&r, T0, over_text(), poi()));
    assert!(green > 0, "the icon must survive the text collision");
    assert_eq!(ink, 0, "the blocked label must not draw");
}

#[test]
fn icon_optional_keeps_the_text_when_the_icon_is_blocked() {
    // The mirror case: `icon-optional` lets the label place without its icon.
    let blocked = |extra: &str| {
        let r = recipe(ROADS_FIRST, extra, "pois");
        counts(&render(&r, T0, over_icon(), poi()))
    };
    let (green, ink) = blocked(&format!("{TEXT}, {ICON}, \"icon-optional\": true"));
    assert_eq!(green, 0, "the blocked icon must not draw");
    assert!(ink > 0, "the label must survive the icon collision");

    // Without the flag the symbol is all-or-nothing again.
    assert_eq!(blocked(&format!("{TEXT}, {ICON}")), (0, 0));
}

#[test]
fn an_icon_box_blocks_another_layers_label() {
    // The icon joins the same index every label collides against: with the
    // POI layer on top, its icon box knocks out a road label that overlaps
    // only the icon — and without the icon that road label draws.
    let with_icon = counts(&render(
        &recipe(POIS_FIRST, &format!("{TEXT}, {ICON}"), "roads"),
        T0,
        over_icon(),
        poi(),
    ));
    let without_icon = counts(&render(
        &recipe(POIS_FIRST, TEXT, "roads"),
        T0,
        over_icon(),
        poi(),
    ));
    assert!(
        without_icon.1 > 0,
        "the road label draws when the POI has no icon"
    );
    assert_eq!(
        with_icon.1, 0,
        "the icon's collision box must knock the road label out"
    );
}

#[test]
fn an_icon_only_symbol_places_through_the_shared_index() {
    // A layer with an icon and no `text-field` still goes through placement:
    // it blocks a lower-priority label, and is itself blocked by a
    // higher-priority one.
    // No `font`, no `text`: the node carries only the icon.
    let icon_only = |order: &str, output: &str| {
        counts(&render(
            &recipe(order, ICON, output),
            T0,
            over_icon(),
            poi(),
        ))
    };
    // POIs on top: the icon draws and takes the road label's box.
    let (green, ink) = icon_only(POIS_FIRST, "both");
    assert!(green > 0, "the icon-only symbol must draw");
    assert_eq!(ink, 0, "its box must block the overlapping road label");
    // Roads on top: the road label wins and the icon drops.
    let (green, ink) = icon_only(ROADS_FIRST, "both");
    assert_eq!(green, 0, "a blocked icon-only symbol must not draw");
    assert!(ink > 0, "the road label keeps its place");
}

#[test]
fn a_symbol_with_an_icon_is_seamless_across_a_tile_border() {
    // Two adjacent tiles decide the symbols straddling their shared edge from
    // the same world-space candidates — icon boxes included — so the border
    // strip must come out byte-for-byte identical.
    let r = recipe(POIS_FIRST, &format!("{TEXT}, {ICON}"), "both");
    // In A's frame the border is x = EXTENT; in B's it is x = 0.
    let pois_a = || vec![point("II", TILE as f32, 64.0)];
    let pois_b = || vec![point("II", 0.0, 64.0)];
    let roads_a = || vec![point("MM", TILE as f32 + 4.0, 70.0)];
    let roads_b = || vec![point("MM", 4.0, 70.0)];

    let left = render_with_features_and_sprite(
        &r,
        TILE,
        PAD,
        TileId { z: 4, x: 5, y: 7 },
        &[
            ("src.roads", layer("roads", roads_a())),
            ("src.pois", layer("pois", pois_a())),
            ("src.roads@1,0", layer("roads", roads_b())),
            ("src.pois@1,0", layer("pois", pois_b())),
        ],
        "atlas",
        dot_sheet(),
    );
    let right = render_with_features_and_sprite(
        &r,
        TILE,
        PAD,
        TileId { z: 4, x: 6, y: 7 },
        &[
            ("src.roads", layer("roads", roads_b())),
            ("src.pois", layer("pois", pois_b())),
            ("src.roads@-1,0", layer("roads", roads_a())),
            ("src.pois@-1,0", layer("pois", pois_a())),
        ],
        "atlas",
        dot_sheet(),
    );

    let mut matched = 0usize;
    for y in 0..left.height {
        for dx in 0..PAD {
            let l = left.pixel(TILE + PAD + dx, y);
            let rr = right.pixel(PAD + dx, y);
            assert_eq!(
                l, rr,
                "seam mismatch at dx={dx}, y={y}: {l:?} vs {rr:?} (tiles disagree)"
            );
            if l[3] > 100 {
                matched += 1;
            }
        }
    }
    assert!(
        matched > 0,
        "expected the symbol's pixels in the seam strip"
    );
}

#[test]
fn icon_text_fit_stretches_the_icon_over_its_label() {
    // `icon-text-fit: both` sizes the icon to the label's box plus the fit
    // padding, so a longer name draws a wider icon — and the icon's own
    // 16 px sprite size stops deciding anything.
    let fields = format!(
        r##"{TEXT}, {ICON}, "icon-text-fit": "both",
            "icon-text-fit-padding": [2, 4, 2, 4]"##
    );
    let fitted = recipe(POIS_FIRST, &fields, "pois");
    let short = counts(&render(&fitted, T0, vec![], vec![point("I", 64.0, 64.0)])).0;
    let long = counts(&render(
        &fitted,
        T0,
        vec![],
        vec![point("IIIIII", 64.0, 64.0)],
    ))
    .0;
    assert!(
        long > short,
        "a longer label must widen the fitted icon: {long} vs {short}"
    );
    // Without the fit both labels get the same 16×16 sprite.
    let plain = recipe(POIS_FIRST, &format!("{TEXT}, {ICON}"), "pois");
    let a = counts(&render(&plain, T0, vec![], vec![point("I", 64.0, 64.0)])).0;
    let b = counts(&render(
        &plain,
        T0,
        vec![],
        vec![point("IIIIII", 64.0, 64.0)],
    ))
    .0;
    assert_eq!(a, b, "an unfitted icon keeps its sprite size");
    assert_eq!(a, 16 * 16);
}

#[test]
fn a_fitted_icon_reserves_the_box_it_covers() {
    // The fitted box is the collision box: with a generous fit padding the
    // icon reaches well past the sprite's 16 px and knocks out a road label
    // that the unfitted symbol leaves alone.
    let fitted = recipe(
        POIS_FIRST,
        &format!(
            r##"{TEXT}, {ICON}, "icon-text-fit": "both", "icon-text-fit-padding": [20, 20, 20, 20]"##
        ),
        "roads",
    );
    let plain = recipe(POIS_FIRST, &format!("{TEXT}, {ICON}"), "roads");
    // Above and right of the POI: clear of both its 16 px icon and its label.
    let road = vec![point("M", 76.0, 40.0)];
    let poi = vec![point("IIIIII", 64.0, 64.0)];
    assert!(
        counts(&render(&plain, T0, road.clone(), poi.clone())).1 > 0,
        "the road label is clear of the unfitted symbol"
    );
    assert_eq!(
        counts(&render(&fitted, T0, road, poi)).1,
        0,
        "the fitted icon's box must block the road label"
    );
}
