//! Drawing a legend entry's symbol through the pipeline that draws the
//! map.

use ezu_graph::{Cache, NoAssets, ParamValues, RasterBuf};
use ezu_paint::legend::{render_swatch, SwatchOptions};
use ezu_paint::nodes::default_registry;
use ezu_style::{Document, LegendEntry, LegendGeometry, NodeRef};

const W: u32 = 64;
const H: u32 = 24;

fn entry(from: &str, props: &[(&str, serde_json::Value)]) -> LegendEntry {
    let mut properties = serde_json::Map::new();
    for (k, v) in props {
        properties.insert((*k).to_string(), v.clone());
    }
    LegendEntry {
        label: format!("swatch of {from}"),
        from: NodeRef(from.to_string()),
        properties,
        note: None,
        min_zoom: None,
        max_zoom: None,
        geometry: None,
    }
}

fn opts() -> SwatchOptions {
    SwatchOptions {
        width: W,
        height: H,
        zoom: 12,
        pad: 0,
        geometry: LegendGeometry::All,
    }
}

/// Draw a swatch and hand back the cropped pixels, so tests index from
/// the swatch's own top-left rather than into the pad.
fn swatch(json: &str, e: &LegendEntry, o: &SwatchOptions) -> Vec<[u8; 4]> {
    swatch_cached(json, e, o, &Cache::new())
}

fn swatch_cached(json: &str, e: &LegendEntry, o: &SwatchOptions, cache: &Cache) -> Vec<[u8; 4]> {
    let doc = Document::from_json(json).expect("parse");
    let registry = default_registry();
    let (buf, canvas) = render_swatch(&doc, e, &registry, &NoAssets, &ParamValues::new(), cache, o)
        .expect("swatch");
    let pad = canvas.pad;
    let mut out = Vec::with_capacity((o.width * o.height) as usize);
    for y in 0..o.height {
        for x in 0..o.width {
            out.push(pixel(&buf, x + pad, y + pad));
        }
    }
    out
}

fn pixel(buf: &RasterBuf, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * buf.width + x) * 4) as usize;
    [
        buf.pixels[i],
        buf.pixels[i + 1],
        buf.pixels[i + 2],
        buf.pixels[i + 3],
    ]
}

fn at(px: &[[u8; 4]], x: u32, y: u32) -> [u8; 4] {
    px[(y * W + x) as usize]
}

/// A style with a basemap under a data-driven fill, a stroke and a dot —
/// the three symbol shapes a legend has to be able to show.
const STYLE: &str = r##"{
  "name": "thematic",
  "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
  "nodes": {
    "bg":    { "op": "solid", "color": "#ffff00" },
    "feats": { "op": "features", "source": "src", "layer": "areas" },
    "area":  { "op": "fill-solid", "features": "@feats", "fill": "#000000",
               "fill-expr": ["match", ["get", "cls"], "a", "#ff0000", "b", "#0000ff", "#888888"] },
    "line":  { "op": "stroke", "features": "@feats", "width-px": 4, "color": "#008000" },
    "dot":   { "op": "circles", "features": "@feats", "radius": 4, "color": "#800080" },
    "out":   { "op": "stack", "layers": ["@bg", "@area", "@line", "@dot"] }
  },
  "output": "@out"
}"##;

#[test]
fn a_fill_entry_shows_the_fill_over_the_whole_swatch() {
    let px = swatch(STYLE, &entry("area", &[("cls", "a".into())]), &opts());
    assert_eq!(px.len(), (W * H) as usize);
    for (x, y) in [
        (0, 0),
        (W - 1, 0),
        (0, H - 1),
        (W - 1, H - 1),
        (W / 2, H / 2),
    ] {
        let p = at(&px, x, y);
        assert!(
            p[0] > 200 && p[1] < 60 && p[3] > 200,
            "({x}, {y}) should be the red fill: {p:?}"
        );
    }
}

/// The basemap is not part of the symbol. Only the entry's own node and
/// what it depends on are rendered, so everywhere the symbol does not
/// cover stays transparent — which is what lets a host place a swatch on
/// its own background.
#[test]
fn a_swatch_carries_nothing_but_its_own_symbol() {
    let px = swatch(STYLE, &entry("dot", &[]), &opts());
    // The dot sits in the middle; the corners are empty, not yellow.
    assert!(at(&px, W / 2, H / 2)[3] > 200, "the dot was not drawn");
    for (x, y) in [(0, 0), (W - 1, 0), (0, H - 1), (W - 1, H - 1)] {
        assert_eq!(at(&px, x, y)[3], 0, "({x}, {y}) should be transparent");
    }
}

/// The test that matters most: two entries differ only in a property, so
/// they share every node and every param hash. If the entry's identity
/// does not reach the cache key, the second reads the first's buffer and
/// a choropleth legend comes out one colour.
#[test]
fn entries_differing_only_in_properties_get_different_swatches() {
    let a = swatch(STYLE, &entry("area", &[("cls", "a".into())]), &opts());
    let b = swatch(STYLE, &entry("area", &[("cls", "b".into())]), &opts());
    let (pa, pb) = (at(&a, W / 2, H / 2), at(&b, W / 2, H / 2));
    assert!(pa[0] > 200 && pa[2] < 60, "first should be red: {pa:?}");
    assert!(pb[2] > 200 && pb[0] < 60, "second should be blue: {pb:?}");
}

/// Every entry of a legend drawn through *one* cache, which is what a
/// host does. The first render leaves its buffers in the cache; the
/// second must not be handed them just because it walks the same nodes
/// with the same parameters. Only the entry's own identity separates
/// them.
#[test]
fn a_shared_cache_does_not_blur_two_entries_together() {
    let cache = Cache::new();
    let red = entry("area", &[("cls", "a".into())]);
    let blue = entry("area", &[("cls", "b".into())]);
    let first = swatch_cached(STYLE, &red, &opts(), &cache);
    let second = swatch_cached(STYLE, &blue, &opts(), &cache);
    let again = swatch_cached(STYLE, &red, &opts(), &cache);
    let (p1, p2, p3) = (at(&first, 2, 2), at(&second, 2, 2), at(&again, 2, 2));
    assert!(p1[0] > 200 && p1[2] < 60, "first should be red: {p1:?}");
    assert!(p2[2] > 200 && p2[0] < 60, "second should be blue: {p2:?}");
    assert_eq!(p1, p3, "the same entry should draw the same swatch");
}

#[test]
fn a_line_entry_draws_across_the_middle() {
    let px = swatch(STYLE, &entry("line", &[]), &opts());
    let opaque_rows: Vec<u32> = (0..H).filter(|&y| at(&px, W / 2, y)[3] > 128).collect();
    assert!(!opaque_rows.is_empty(), "the line was not drawn");
    let centre = opaque_rows.iter().sum::<u32>() / opaque_rows.len() as u32;
    assert!(
        centre.abs_diff(H / 2) <= 2,
        "line centred on row {centre}, expected about {}",
        H / 2
    );
    // Across the full width, and green.
    assert!(at(&px, 1, centre)[1] > 100 && at(&px, W - 2, centre)[1] > 100);
}

/// Restricting the geometry is how an entry stops a node from drawing
/// twice when a geometry op sits in between. Asked for a point only, a
/// stroke node has no line to draw.
#[test]
fn geometry_restricts_what_the_node_is_given() {
    let point_only = SwatchOptions {
        geometry: LegendGeometry::Point,
        ..opts()
    };
    let px = swatch(STYLE, &entry("line", &[]), &point_only);
    assert!(
        px.iter().all(|p| p[3] == 0),
        "a stroke node given no lines should draw nothing"
    );
    // The dot still draws from the same restricted feature.
    let px = swatch(STYLE, &entry("dot", &[]), &point_only);
    assert!(at(&px, W / 2, H / 2)[3] > 200);
}

/// An entry that names its own geometry overrides whatever default the
/// caller renders with, so one awkward entry can be fixed in the style
/// without changing the rest.
#[test]
fn an_entry_may_name_its_own_geometry() {
    let mut e = entry("line", &[]);
    e.geometry = Some(LegendGeometry::Point);
    // The default here offers all three geometries; the entry asks for a
    // point only, so the stroke node is left with no line.
    let px = swatch(STYLE, &e, &opts());
    assert!(
        px.iter().all(|p| p[3] == 0),
        "the entry's own geometry should have won"
    );
}

/// A zoom curve is read at the zoom the swatch was asked for, so a
/// symbol that fades out with scale shows that. The curve lives on an
/// `expr` node feeding the fill, which the subgraph has to bring along
/// or there would be nothing to fade.
#[test]
fn the_zoom_the_swatch_is_asked_for_drives_its_curves() {
    let json = r##"{
      "name": "zoomed",
      "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
      "nodes": {
        "fade":  { "op": "expr", "expr": ["interpolate", ["linear"], ["zoom"], 8, 0, 14, 1] },
        "feats": { "op": "features", "source": "src", "layer": "areas" },
        "area":  { "op": "fill-solid", "features": "@feats", "fill": "#000000",
                   "fill-alpha": "@fade" }
      },
      "output": "@area"
    }"##;
    let low = SwatchOptions { zoom: 8, ..opts() };
    let high = SwatchOptions { zoom: 14, ..opts() };
    let faint = swatch(json, &entry("area", &[]), &low);
    let solid = swatch(json, &entry("area", &[]), &high);
    assert!(
        at(&faint, W / 2, H / 2)[3] < 40,
        "at z8 the fill should be nearly invisible: {:?}",
        at(&faint, W / 2, H / 2)
    );
    assert!(
        at(&solid, W / 2, H / 2)[3] > 200,
        "at z14 the fill should be opaque: {:?}",
        at(&solid, W / 2, H / 2)
    );
}

#[test]
fn an_entry_naming_no_node_is_an_error() {
    let doc = Document::from_json(STYLE).unwrap();
    let registry = default_registry();
    let e = entry("nope", &[]);
    let err = render_swatch(
        &doc,
        &e,
        &registry,
        &NoAssets,
        &ParamValues::new(),
        &Cache::new(),
        &opts(),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("nope"),
        "error should name the node: {err}"
    );
}
