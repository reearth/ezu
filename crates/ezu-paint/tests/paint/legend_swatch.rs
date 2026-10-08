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
        features: None,
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

/// Absolute `file:` URL of the ezu-core digits test font — a house
/// number is all digits — forward-slashed so it embeds into JSON
/// verbatim on every platform.
fn font_url() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.digits.ttf");
    format!("file:{}", path.display()).replace('\\', "/")
}

/// Swatch size for the composite symbol: wide enough for its parts to
/// sit apart, tall enough for a label over a point.
const CW: u32 = 96;
const CH: u32 = 48;

/// Draw a `CW`×`CH` swatch with a loader that reads `file:` fonts, and
/// return the cropped pixels row by row.
fn composite_swatch(json: &str, e: &LegendEntry, cache: &Cache) -> Vec<[u8; 4]> {
    let doc = Document::from_json(json).expect("parse");
    // The legend is checked with the graph, as `ezu check` checks it.
    ezu_graph::build_graph(&doc, &default_registry()).expect("build");
    let o = SwatchOptions {
        width: CW,
        height: CH,
        ..opts()
    };
    let assets = ezu_paint::host::BrushBankLoader::new();
    let (buf, canvas) = render_swatch(
        &doc,
        e,
        &default_registry(),
        &assets,
        &ParamValues::new(),
        cache,
        &o,
    )
    .expect("swatch");
    let mut out = Vec::with_capacity((CW * CH) as usize);
    for y in 0..CH {
        for x in 0..CW {
            out.push(pixel(&buf, x + canvas.pad, y + canvas.pad));
        }
    }
    out
}

/// How many pixels in `[x0, x1) × [y0, y1)` are the label's blue.
fn blue_in(px: &[[u8; 4]], x0: u32, x1: u32, y0: u32, y1: u32) -> usize {
    (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let p = px[(y * CW + x) as usize];
            p[2] > 120 && p[0] < 80 && p[1] < 80 && p[3] > 120
        })
        .count()
}

/// A composite symbol: a building square, a dot at its representative
/// point, a dashed connector to an entrance dot, and the house number
/// over the representative point. Each part is its own `features` node
/// filtering one layer, as a map's layers would. `rep` is where the
/// representative point and its label sit.
fn composite_style(rep: [f64; 2]) -> String {
    format!(
        r##"{{
      "name": "ledger",
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "bldg":  {{ "op": "features", "source": "src", "layer": "buildings",
                    "filter-expr": ["==", ["geometry-type"], "Polygon"] }},
        "conn":  {{ "op": "features", "source": "src", "layer": "addresses",
                    "filter-expr": ["==", ["get", "part"], "connector"] }},
        "pts":   {{ "op": "features", "source": "src", "layer": "addresses",
                    "filter-expr": ["==", ["geometry-type"], "Point"] }},
        "rep":   {{ "op": "features", "source": "src", "layer": "addresses",
                    "filter-expr": ["==", ["get", "part"], "rep"] }},
        "fill":  {{ "op": "fill-solid", "features": "@bldg", "fill": "#c0c0c0" }},
        "line":  {{ "op": "stroke", "features": "@conn", "width-px": 2, "color": "#00a000",
                    "dasharray": [2, 1] }},
        "dots":  {{ "op": "circles", "features": "@pts", "radius": 3, "color": "#ff0000" }},
        "num":   {{ "op": "text", "features": "@rep", "font": ["body"], "size": 14,
                    "text": ["get", "no"], "color": "#0000ff", "anchor": "bottom",
                    "source": "src", "layer": "addresses",
                    "filter-expr": ["==", ["get", "part"], "rep"] }},
        "addr":  {{ "op": "stack", "layers": ["@fill", "@line", "@dots", "@num"] }}
      }},
      "legend": {{ "entries": [{{ "label": "assigned house number", "from": "@addr",
        "properties": {{ "no": "12" }},
        "features": [
          {{ "geometry": {{ "type": "Polygon",
                           "coordinates": [[[0.06, 0.5], [0.42, 0.5], [0.42, 0.95], [0.06, 0.95]]] }} }},
          {{ "geometry": {{ "type": "Point", "coordinates": [{rx}, {ry}] }},
             "properties": {{ "part": "rep" }} }},
          {{ "geometry": {{ "type": "LineString", "coordinates": [[0.24, 0.72], [0.88, 0.72]] }},
             "properties": {{ "part": "connector" }} }},
          {{ "geometry": {{ "type": "Point", "coordinates": [0.88, 0.72] }},
             "properties": {{ "part": "entrance" }} }}
        ] }}] }},
      "output": "@addr"
    }}"##,
        font = font_url(),
        rx = rep[0],
        ry = rep[1],
    )
}

fn composite_entry(json: &str) -> LegendEntry {
    Document::from_json(json)
        .unwrap()
        .legend
        .unwrap()
        .entries
        .remove(0)
}

/// The stand-ins reach each `features` node of the stack together, and
/// each filter picks out its own part: the fill finds the polygon, the
/// stroke the connector, the circles both points, the text the
/// representative point — with the number it carries from the entry.
#[test]
fn declared_stand_ins_draw_a_composite_symbol() {
    let json = composite_style([0.24, 0.72]);
    let px = composite_swatch(&json, &composite_entry(&json), &Cache::new());
    let at = |fx: f32, fy: f32| {
        let (x, y) = ((fx * CW as f32) as u32, (fy * CH as f32) as u32);
        px[(y * CW + x) as usize]
    };

    let building = at(0.1, 0.9);
    assert!(
        building[3] > 200 && building[0].abs_diff(192) < 16 && building[2].abs_diff(192) < 16,
        "the building square should be grey: {building:?}"
    );
    for (fx, name) in [(0.24, "representative point"), (0.88, "entrance")] {
        let p = at(fx, 0.72);
        assert!(
            p[0] > 200 && p[1] < 80 && p[2] < 80,
            "the {name} dot should be red: {p:?}"
        );
    }
    let row = (0.72 * CH as f32) as u32;
    let green = (CW / 2..CW * 4 / 5)
        .filter(|&x| {
            let p = px[(row * CW + x) as usize];
            p[1] > 100 && p[0] < 60 && p[2] < 60
        })
        .count();
    assert!(
        green > 4,
        "the connector should cross to the entrance: {green} px"
    );

    // The number: drawn, above its point, and nowhere over the
    // connector's half of the swatch.
    let label = blue_in(&px, 0, CW / 2, 0, row);
    assert!(label > 20, "the house number should be drawn: {label} px");
    assert_eq!(
        blue_in(&px, CW / 2, CW, 0, CH),
        0,
        "the label strayed right"
    );
    assert_eq!(
        blue_in(&px, 0, CW, row + 2, CH),
        0,
        "the label should sit above its point"
    );
}

/// A swatch has no neighbours. The renderer asks for the eight around a
/// tile to collide and to draw labels across seams; answered with copies
/// of the stand-in, a label cut by one edge would come back in through
/// the opposite one.
#[test]
fn a_label_cut_by_the_edge_does_not_come_back_from_the_opposite_edge() {
    let json = composite_style([0.97, 0.6]);
    let px = composite_swatch(&json, &composite_entry(&json), &Cache::new());
    assert!(
        blue_in(&px, CW * 3 / 4, CW, 0, CH) > 0,
        "the label's own half should be drawn"
    );
    assert_eq!(
        blue_in(&px, 0, CW / 4, 0, CH),
        0,
        "a neighbour's copy of the label was drawn"
    );
}

/// Where the stand-ins sit is part of a swatch's identity: two entries
/// naming the same node with the same properties but placing their
/// features differently must not share a cached buffer.
#[test]
fn entries_differing_only_in_stand_ins_get_different_swatches() {
    let json = composite_style([0.24, 0.72]);
    let cache = Cache::new();
    let first = composite_entry(&json);
    let mut second = first.clone();
    if let Some(features) = second.features.as_mut() {
        features[3].geometry = ezu_style::LegendFeatureGeometry::Point {
            coordinates: [0.6, 0.72],
        };
    }
    let a = composite_swatch(&json, &first, &cache);
    let b = composite_swatch(&json, &second, &cache);
    assert_ne!(a, b, "the moved entrance should draw a different swatch");
}

/// The stand-ins are fractions of the swatch, so one entry fits any
/// swatch size: drawn twice as large, the entrance dot is still at the
/// same fraction of the way across.
#[test]
fn stand_ins_scale_with_the_swatch() {
    let json = composite_style([0.24, 0.72]);
    let e = composite_entry(&json);
    let doc = Document::from_json(&json).unwrap();
    let o = SwatchOptions {
        width: CW * 2,
        height: CH * 2,
        ..opts()
    };
    let (buf, canvas) = render_swatch(
        &doc,
        &e,
        &default_registry(),
        &ezu_paint::host::BrushBankLoader::new(),
        &ParamValues::new(),
        &Cache::new(),
        &o,
    )
    .expect("swatch");
    let p = pixel(
        &buf,
        (0.88 * (CW * 2) as f32) as u32 + canvas.pad,
        (0.72 * (CH * 2) as f32) as u32 + canvas.pad,
    );
    assert!(
        p[0] > 200 && p[1] < 80,
        "the entrance dot should scale along: {p:?}"
    );
}

/// The composite symbol again, but with the connector drawn the way the
/// map draws it: `segment-to` from the entrance to the representative
/// point its feature names by longitude and latitude, here written as
/// places in the swatch. The entrance sits up and to the right, so the
/// connector runs on a diagonal and both axes have to come out right.
fn connector_style() -> String {
    format!(
        r##"{{
      "name": "ledger",
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "bldg":  {{ "op": "features", "source": "src", "layer": "buildings",
                    "filter-expr": ["==", ["geometry-type"], "Polygon"] }},
        "pts":   {{ "op": "features", "source": "src", "layer": "addresses",
                    "filter-expr": ["==", ["geometry-type"], "Point"] }},
        "ent":   {{ "op": "features", "source": "src", "layer": "addresses",
                    "filter-expr": ["==", ["get", "part"], "entrance"] }},
        "rep":   {{ "op": "features", "source": "src", "layer": "addresses",
                    "filter-expr": ["==", ["get", "part"], "rep"] }},
        "conn":  {{ "op": "segment-to", "features": "@ent",
                    "lng-field": "rep_lng", "lat-field": "rep_lat" }},
        "fill":  {{ "op": "fill-solid", "features": "@bldg", "fill": "#c0c0c0" }},
        "line":  {{ "op": "stroke", "features": "@conn", "width-px": 2, "color": "#00a000" }},
        "dots":  {{ "op": "circles", "features": "@pts", "radius": 3, "color": "#ff0000" }},
        "num":   {{ "op": "text", "features": "@rep", "font": ["body"], "size": 14,
                    "text": ["get", "no"], "color": "#0000ff", "anchor": "bottom",
                    "source": "src", "layer": "addresses",
                    "filter-expr": ["==", ["get", "part"], "rep"] }},
        "addr":  {{ "op": "stack", "layers": ["@fill", "@line", "@dots", "@num"] }}
      }},
      "legend": {{ "entries": [{{ "label": "assigned house number", "from": "@addr",
        "properties": {{ "no": "12" }},
        "features": [
          {{ "geometry": {{ "type": "Polygon",
                           "coordinates": [[[0.06, 0.5], [0.42, 0.5], [0.42, 0.95], [0.06, 0.95]]] }} }},
          {{ "geometry": {{ "type": "Point", "coordinates": [0.24, 0.72] }},
             "properties": {{ "part": "rep" }} }},
          {{ "geometry": {{ "type": "Point", "coordinates": [0.88, 0.3] }},
             "properties": {{ "part": "entrance",
                             "rep_lng": {{ "swatch-x": 0.24 }}, "rep_lat": {{ "swatch-y": 0.72 }} }} }}
        ] }}] }},
      "output": "@addr"
    }}"##,
        font = font_url(),
    )
}

/// Draw the connector swatch at `scale` times the composite size and
/// return its cropped pixels with its width and height.
fn connector_swatch(scale: u32) -> (Vec<[u8; 4]>, u32, u32) {
    let json = connector_style();
    let doc = Document::from_json(&json).expect("parse");
    ezu_graph::build_graph(&doc, &default_registry()).expect("build");
    let (w, h) = (CW * scale, CH * scale);
    let o = SwatchOptions {
        width: w,
        height: h,
        ..opts()
    };
    let (buf, canvas) = render_swatch(
        &doc,
        &composite_entry(&json),
        &default_registry(),
        &ezu_paint::host::BrushBankLoader::new(),
        &ParamValues::new(),
        &Cache::new(),
        &o,
    )
    .expect("swatch");
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            out.push(pixel(&buf, x + canvas.pad, y + canvas.pad));
        }
    }
    (out, w, h)
}

/// Whether any pixel within one of the point `(fx, fy)` — fractions of
/// the swatch — is the connector's green.
fn green_near(px: &[[u8; 4]], w: u32, h: u32, fx: f64, fy: f64) -> bool {
    let (cx, cy) = ((fx * w as f64) as i64, (fy * h as f64) as i64);
    (-1..=1).any(|dy| {
        (-1..=1).any(|dx| {
            let (x, y) = (cx + dx, cy + dy);
            if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
                return false;
            }
            let p = px[(y as u32 * w + x as u32) as usize];
            p[1] > 100 && p[0] < 60 && p[2] < 60 && p[3] > 120
        })
    })
}

/// The connector is the map's own `segment-to` node, and it runs between
/// the two places the entry declares: the entrance dot, and the
/// representative point the entrance's properties name in swatch
/// fractions. Checked near both ends and in the middle, and away from
/// the diagonal, at the composite size and at twice it.
#[test]
fn segment_to_draws_between_declared_swatch_positions() {
    let (rep, ent) = ([0.24, 0.72], [0.88, 0.3]);
    let along = |t: f64| {
        (
            rep[0] + (ent[0] - rep[0]) * t,
            rep[1] + (ent[1] - rep[1]) * t,
        )
    };
    for scale in [1, 2] {
        let (px, w, h) = connector_swatch(scale);
        for t in [0.15, 0.5, 0.85] {
            let (fx, fy) = along(t);
            assert!(
                green_near(&px, w, h, fx, fy),
                "{scale}x: the connector should pass ({fx:.3}, {fy:.3})"
            );
        }
        // Not drawn level with either end, as it would be if one axis
        // were lost.
        let (mx, _) = along(0.5);
        for fy in [rep[1], ent[1]] {
            assert!(
                !green_near(&px, w, h, mx, fy),
                "{scale}x: the connector strayed to row {fy}"
            );
        }
        // The parts it joins are drawn too, over its ends.
        for [fx, fy] in [rep, ent] {
            let p = px[((fy * h as f64) as u32 * w + (fx * w as f64) as u32) as usize];
            assert!(
                p[0] > 200 && p[1] < 80,
                "{scale}x: dot at ({fx}, {fy}): {p:?}"
            );
        }
        let label = (0..h * 72 / 100)
            .flat_map(|y| (0..w / 2).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let p = px[(y * w + x) as usize];
                p[2] > 120 && p[0] < 80 && p[1] < 80 && p[3] > 120
            })
            .count();
        assert!(label > 20, "{scale}x: the house number should be drawn");
    }
}

/// A malformed swatch position is refused before anything is drawn, by
/// the swatch as well as by the graph build that `ezu check` runs.
#[test]
fn a_malformed_swatch_position_is_refused() {
    let json = connector_style().replace(r#""swatch-x": 0.24"#, r#""swatch-x": 1.24"#);
    let doc = Document::from_json(&json).expect("parse");
    let err = ezu_graph::build_graph(&doc, &default_registry()).unwrap_err();
    assert!(
        matches!(err, ezu_graph::BuildGraphError::LegendFeatures { .. }),
        "{err:?}"
    );
    let err = render_swatch(
        &doc,
        &composite_entry(&json),
        &default_registry(),
        &NoAssets,
        &ParamValues::new(),
        &Cache::new(),
        &opts(),
    )
    .unwrap_err();
    assert!(
        matches!(err, ezu_paint::legend::SwatchError::Features { .. }),
        "{err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("`features[2].properties.rep_lng`: `swatch-x` is 1.24"),
        "{msg}"
    );
}
