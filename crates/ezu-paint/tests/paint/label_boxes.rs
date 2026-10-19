//! `label-boxes`: the boxes a `label-placement` node measured its labels by,
//! as polygons. Each test reads the emitted features directly and, where a
//! box has a drawn counterpart, checks it against the pixels `text-draw`
//! puts down.

use crate::common::features_of_node;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::{RasterBuf, TileId};
use ezu_paint::nodes::FilteredFeatures;
use std::collections::HashMap;

/// Tile size in px; the emitted geometry's default extent is 4096, so one
/// px is 64 units.
const TILE: u32 = 64;
const UNITS_PER_PX: f32 = 4096.0 / TILE as f32;

fn font_url() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
    format!("file:{}", path.display()).replace('\\', "/")
}

fn feature(name: &str, geometry: Geometry) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("name".to_string(), Value::String(name.to_string()));
    Feature {
        id: None,
        geometry,
        properties,
    }
}

fn point(name: &str, x: i32, y: i32) -> Feature {
    let mut g = Geometry::default();
    g.points.push((x, y));
    feature(name, g)
}

fn line(name: &str, pts: &[(i32, i32)]) -> Feature {
    let mut g = Geometry::default();
    g.lines.push(pts.to_vec());
    feature(name, g)
}

fn layer(features: Vec<Feature>) -> FeatureLayer {
    FeatureLayer {
        name: "pts".to_string(),
        extent: 4096,
        features,
    }
}

/// White 16 px labels through `text-labels` → `label-placement`, drawn by
/// `text-draw` over the `label-boxes` polygons filled blue. `text_extra` and
/// `box_extra` add fields to the label layer and the `boxes` node.
fn recipe(text_extra: &str, box_extra: &str) -> String {
    format!(
        r##"{{
      "name": "label-boxes",
      "tile-size": {TILE},
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats":  {{ "op": "features", "source": "src", "layer": "pts" }},
        "labels": {{ "op": "text-labels", "features": "@feats", "source": "src", "layer": "pts",
                     "font": ["body"], "text": ["get", "name"], "size": 16,
                     "color": "#ffffff" {text_extra} }},
        "placed": {{ "op": "label-placement", "labels": ["@labels"] }},
        "boxes":  {{ "op": "label-boxes", "placement": "@placed" {box_extra} }},
        "fill":   {{ "op": "fill-solid", "features": "@boxes", "fill": "#0000ff" }},
        "text":   {{ "op": "text-draw", "labels": "@labels", "placement": "@placed" }},
        "out":    {{ "op": "stack", "layers": ["@fill", "@text"] }}
      }},
      "output": "@out"
    }}"##,
        font = font_url(),
    )
}

fn run(
    recipe: &str,
    tile: TileId,
    bindings: &[(&str, FeatureLayer)],
) -> (std::sync::Arc<FilteredFeatures>, std::sync::Arc<RasterBuf>) {
    features_of_node(recipe, TILE, 0, tile, bindings, "boxes")
}

fn run_centre(recipe: &str, features: Vec<Feature>) -> std::sync::Arc<FilteredFeatures> {
    run(
        recipe,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer(features))],
    )
    .0
}

/// One emitted box: its ring in px, and the properties it carries.
struct Box {
    ring: Vec<(f32, f32)>,
    props: std::sync::Arc<std::collections::BTreeMap<String, maplibre_expr::Value>>,
}

impl Box {
    fn part(&self) -> &str {
        match self.props.get("part") {
            Some(maplibre_expr::Value::String(s)) => s,
            other => panic!("part: {other:?}"),
        }
    }
    fn placed(&self) -> bool {
        match self.props.get("placed") {
            Some(maplibre_expr::Value::Bool(b)) => *b,
            other => panic!("placed: {other:?}"),
        }
    }
    fn label_id(&self) -> f64 {
        match self.props.get("label-id") {
            Some(maplibre_expr::Value::Number(n)) => *n,
            other => panic!("label-id: {other:?}"),
        }
    }
    fn name(&self) -> &str {
        match self.props.get("name") {
            Some(maplibre_expr::Value::String(s)) => s,
            other => panic!("name: {other:?}"),
        }
    }
    /// `(min_x, min_y, max_x, max_y)` in px.
    fn bounds(&self) -> (f32, f32, f32, f32) {
        self.ring.iter().fold(
            (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
            |(x0, y0, x1, y1), &(x, y)| (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
        )
    }
}

fn boxes(feats: &FilteredFeatures) -> Vec<Box> {
    feats
        .groups
        .iter()
        .flat_map(|g| {
            g.polygons.iter().map(|p| Box {
                ring: p
                    .exterior
                    .iter()
                    .map(|&(x, y)| (x as f32 / UNITS_PER_PX, y as f32 / UNITS_PER_PX))
                    .collect(),
                props: g.properties.clone(),
            })
        })
        .collect()
}

fn of_part<'a>(all: &'a [Box], part: &str) -> Vec<&'a Box> {
    all.iter().filter(|b| b.part() == part).collect()
}

/// The inclusive pixel bounds of every pixel `keep` selects.
fn pixel_bounds(r: &RasterBuf, keep: impl Fn([u8; 4]) -> bool) -> Option<(u32, u32, u32, u32)> {
    let mut b: Option<(u32, u32, u32, u32)> = None;
    for y in 0..r.height {
        for x in 0..r.width {
            if keep(r.pixel(x, y)) {
                b = Some(match b {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
    }
    b
}

/// Whether two px bounds agree to within one extent unit.
fn close(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> bool {
    let eps = 1.5 / UNITS_PER_PX;
    (a.0 - b.0).abs() <= eps
        && (a.1 - b.1).abs() <= eps
        && (a.2 - b.2).abs() <= eps
        && (a.3 - b.3).abs() <= eps
}

#[test]
fn a_point_label_gives_its_text_block_and_its_padded_collision_box() {
    let (feats, raster) = run(
        &recipe(r#", "padding-px": 4"#, ""),
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer(vec![point("HH", 2048, 2048)]))],
    );
    let all = boxes(&feats);
    let text = of_part(&all, "text");
    let collision = of_part(&all, "collision");
    assert_eq!((text.len(), collision.len()), (1, 1));
    assert!(of_part(&all, "background").is_empty(), "no background set");
    assert!(of_part(&all, "icon").is_empty(), "no icon set");
    let t = text[0].bounds();
    // The text block is centred on the point (32, 32) and holds every glyph.
    assert!(
        t.0 < 32.0 && t.2 > 32.0 && t.1 < 32.0 && t.3 > 32.0,
        "{t:?}"
    );
    let (gx0, gy0, gx1, gy1) = pixel_bounds(&raster, |p| p[1] > 0).expect("the text draws");
    assert!(
        t.0 <= gx0 as f32
            && gx1 as f32 + 1.0 <= t.2
            && t.1 <= gy0 as f32
            && gy1 as f32 + 1.0 <= t.3,
        "glyphs ({gx0}, {gy0})–({gx1}, {gy1}) inside the text block {t:?}"
    );
    // The collision box is the text block grown by `padding-px`.
    let c = collision[0].bounds();
    assert!(
        close(c, (t.0 - 4.0, t.1 - 4.0, t.2 + 4.0, t.3 + 4.0)),
        "{c:?} vs {t:?}"
    );
    // Both carry the feature's properties and the same label.
    for b in [text[0], collision[0]] {
        assert!(b.placed());
        assert_eq!(b.name(), "HH");
    }
    assert_eq!(text[0].label_id(), collision[0].label_id());
}

#[test]
fn parts_picks_which_boxes_are_emitted() {
    let all = boxes(&run_centre(
        &recipe("", r#", "parts": ["collision"]"#),
        vec![point("HH", 2048, 2048)],
    ));
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].part(), "collision");
}

#[test]
fn the_background_part_is_the_box_text_draw_fills() {
    let (feats, raster) = run(
        &recipe(
            r##", "anchor": "top-left", "background-color": "#ff0000",
               "background-padding": [3, 5, 3, 5]"##,
            "",
        ),
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer(vec![point("HH", 1024, 1024)]))],
    );
    let all = boxes(&feats);
    let bg = of_part(&all, "background");
    assert_eq!(bg.len(), 1);
    let b = bg[0].bounds();
    // `top-left` puts the box corner on the point.
    assert!(close((b.0, b.1, 0.0, 0.0), (16.0, 16.0, 0.0, 0.0)), "{b:?}");
    // `text-draw` fills the same box, its edges snapped to whole pixels.
    let drawn =
        pixel_bounds(&raster, |p| p[0] == 255 && p[1] < 255 && p[2] == 0).expect("the box draws");
    assert_eq!(
        drawn,
        (
            b.0.round() as u32,
            b.1.round() as u32,
            b.2.round() as u32 - 1,
            b.3.round() as u32 - 1
        ),
        "drawn box vs background part {b:?}"
    );
    // The text block sits inside it by the padding; the collision box (with
    // a background, the background itself) is padded by the default 2 px.
    let t = of_part(&all, "text")[0].bounds();
    assert!(
        close(t, (b.0 + 5.0, b.1 + 3.0, b.2 - 5.0, b.3 - 3.0)),
        "{t:?} in {b:?}"
    );
    let c = of_part(&all, "collision")[0].bounds();
    assert!(
        close(c, (b.0 - 2.0, b.1 - 2.0, b.2 + 2.0, b.3 + 2.0)),
        "{c:?} around {b:?}"
    );
}

#[test]
fn padding_alone_gives_the_same_background_part_without_painting_it() {
    let padded = r##", "anchor": "top-left", "background-padding": [3, 5, 3, 5]"##;
    let filled = format!(r##"{padded}, "background-color": "#ff0000""##);
    let (bare, raster) = run(
        &recipe(padded, ""),
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer(vec![point("HH", 1024, 1024)]))],
    );
    let bare = boxes(&bare);
    let filled = boxes(&run_centre(
        &recipe(&filled, ""),
        vec![point("HH", 1024, 1024)],
    ));
    // Every part matches the coloured label's, the background included.
    for part in ["text", "background", "collision"] {
        let (b, f) = (of_part(&bare, part), of_part(&filled, part));
        assert_eq!(b.len(), 1, "{part}");
        assert!(
            close(b[0].bounds(), f[0].bounds()),
            "{part}: {:?} vs {:?}",
            b[0].bounds(),
            f[0].bounds()
        );
    }
    // `text-draw` paints no box: nothing red.
    assert!(
        pixel_bounds(&raster, |p| p[0] == 255 && p[1] < 255 && p[2] == 0).is_none(),
        "an unpainted box"
    );
}

/// One label's `(name, variant-index, overlapped, drop-reason)`.
type Outcome = (String, Option<f64>, Option<bool>, Option<String>);

/// Each label's [`Outcome`], read off its `text` box (the collision box
/// for one that lost).
fn outcomes(all: &[Box]) -> Vec<Outcome> {
    let mut v: Vec<_> = all
        .iter()
        .filter(|b| b.part() == if b.placed() { "text" } else { "collision" })
        .map(|b| {
            let num = |k: &str| match b.props.get(k) {
                Some(maplibre_expr::Value::Number(n)) => Some(*n),
                None => None,
                other => panic!("{k}: {other:?}"),
            };
            let flag = |k: &str| match b.props.get(k) {
                Some(maplibre_expr::Value::Bool(f)) => Some(*f),
                None => None,
                other => panic!("{k}: {other:?}"),
            };
            let reason = match b.props.get("drop-reason") {
                Some(maplibre_expr::Value::String(s)) => Some(s.clone()),
                None => None,
                other => panic!("drop-reason: {other:?}"),
            };
            (
                b.name().to_string(),
                num("variant-index"),
                flag("overlapped"),
                reason,
            )
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

#[test]
fn a_placed_label_names_the_anchor_it_took() {
    // Two labels on one point: the first takes the first anchor, the second
    // moves to the next free one.
    let feats = vec![point("A", 2048, 2048), point("B", 2048, 2048)];
    let all = boxes(&run_centre(
        &recipe(
            r#", "anchor-variants": ["top", "bottom"], "radial-offset": 1.0"#,
            "",
        ),
        feats,
    ));
    let got = outcomes(&all);
    let indices: Vec<Option<f64>> = got.iter().map(|o| o.1).collect();
    let mut sorted = indices.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(sorted, [Some(0.0), Some(1.0)], "{got:?}");
    assert!(
        got.iter().all(|o| o.2 == Some(false) && o.3.is_none()),
        "{got:?}"
    );
}

#[test]
fn an_allowed_overlap_is_marked_and_a_lost_label_says_why() {
    let feats = || vec![point("A", 2048, 2048), point("B", 2048, 2048)];
    let all = boxes(&run_centre(
        &recipe(r#", "allow-overlap": true"#, ""),
        feats(),
    ));
    let got = outcomes(&all);
    let overlapped: Vec<Option<bool>> = got.iter().map(|o| o.2).collect();
    let mut sorted = overlapped.clone();
    sorted.sort();
    assert_eq!(sorted, [Some(false), Some(true)], "{got:?}");
    assert!(got.iter().all(|o| o.1 == Some(0.0)), "{got:?}");

    let all = boxes(&run_centre(&recipe("", r#", "include": "all""#), feats()));
    let got = outcomes(&all);
    let lost: Vec<_> = got.iter().filter(|o| o.3.is_some()).collect();
    assert_eq!(lost.len(), 1, "{got:?}");
    assert_eq!(lost[0].3.as_deref(), Some("collision"));
    assert_eq!((lost[0].1, lost[0].2), (None, None), "{got:?}");
}

#[test]
fn include_all_adds_the_candidates_that_lost_unplaced() {
    // Two boxed labels close enough that only one places.
    let feats = || vec![point("I", 1536, 2048), point("I", 2560, 2048)];
    let extra = r##", "background-color": "#ff0000", "background-padding": [0, 8, 0, 8]"##;
    let placed = boxes(&run_centre(&recipe(extra, ""), feats()));
    let ids = |bs: &[Box], placed: bool| {
        let mut v: Vec<f64> = bs
            .iter()
            .filter(|b| b.placed() == placed)
            .map(Box::label_id)
            .collect();
        v.sort_by(f64::total_cmp);
        v.dedup();
        v
    };
    assert_eq!(ids(&placed, true).len(), 1, "one label places");
    assert!(ids(&placed, false).is_empty(), "no losers by default");

    let all = boxes(&run_centre(
        &recipe(extra, r#", "include": "all""#),
        feats(),
    ));
    assert_eq!(ids(&all, true), ids(&placed, true), "the same winner");
    let lost = ids(&all, false);
    assert_eq!(lost.len(), 1, "the loser comes through unplaced");
    // The loser's collision box overlaps the winner's: that is why it lost.
    let collision = |placed: bool| {
        all.iter()
            .find(|b| b.part() == "collision" && b.placed() == placed)
            .expect("a collision box")
            .bounds()
    };
    let (w, l) = (collision(true), collision(false));
    assert!(
        w.0 < l.2 && l.0 < w.2 && w.1 < l.3 && l.1 < w.3,
        "{w:?} {l:?}"
    );
}

#[test]
fn a_line_label_gives_a_rotated_quad_and_a_collision_box_per_glyph() {
    // A diagonal through the tile; "MMMM" lays four glyphs along it.
    let all = boxes(&run_centre(
        &recipe(r#", "placement": "line-center""#, ""),
        vec![line("MMMM", &[(0, 0), (4096, 4096)])],
    ));
    let glyphs = of_part(&all, "glyph");
    let collision = of_part(&all, "collision");
    assert_eq!((glyphs.len(), collision.len()), (4, 4));
    assert!(
        of_part(&all, "text").is_empty(),
        "a line label has no single block"
    );
    for (g, c) in glyphs.iter().zip(&collision) {
        // Each quad runs along the 45° path: its first edge points down-right.
        let (a, b) = (g.ring[0], g.ring[1]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        assert!(
            dx > 0.0 && (dy - dx).abs() < 0.05 * dx,
            "glyph edge ({dx}, {dy})"
        );
        // The collision box is the glyph's axis-aligned extent, padded.
        let (cx0, cy0, cx1, cy1) = c.bounds();
        let (gx0, gy0, gx1, gy1) = g.bounds();
        assert!(
            cx0 < gx0 && cy0 < gy0 && gx1 < cx1 && gy1 < cy1,
            "glyph {:?} inside its collision box {:?}",
            g.bounds(),
            c.bounds()
        );
        assert_eq!(g.label_id(), c.label_id());
    }
}

#[test]
fn a_label_straddling_the_seam_gives_the_same_boxes_on_both_tiles() {
    // The label's own tile is x=5; tile x=6 sees it only as a neighbour.
    let feats = || layer(vec![point("HHH", 3800, 2048)]);
    let rc = recipe(
        r##", "background-color": "#2f6fe0", "background-padding": [3, 6, 3, 6]"##,
        "",
    );
    let (left, _) = run(&rc, TileId { z: 4, x: 5, y: 7 }, &[("src.pts", feats())]);
    let (right, _) = run(
        &rc,
        TileId { z: 4, x: 6, y: 7 },
        &[("src.pts@-1,0", feats())],
    );
    let (left, right) = (boxes(&left), boxes(&right));
    assert!(!right.is_empty(), "the neighbour's label reaches this tile");
    for part in ["text", "background", "collision"] {
        let l = of_part(&left, part);
        let r = of_part(&right, part);
        assert_eq!((l.len(), r.len()), (1, 1), "{part}");
        let (lb, rb) = (l[0].bounds(), r[0].bounds());
        // The right tile's frame starts one tile further right.
        let w = TILE as f32;
        assert!(
            close((lb.0 - w, lb.1, lb.2 - w, lb.3), rb),
            "{part}: {lb:?} vs {rb:?}"
        );
        assert!(lb.2 > w, "{part} crosses the seam: {lb:?}");
        assert_eq!(l[0].label_id(), r[0].label_id(), "{part}: same label");
    }
}
