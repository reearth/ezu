//! Rendering from a `SourceSet` draws what binding the same bytes to the
//! renderer draws, for every tile-scoped kind, and lets one loaded tile
//! serve several renders.

#![cfg(feature = "renderer")]

use ezu::renderer::{
    BindOptions, ErrorKind, OutputFormat, RenderOptions, Renderer, SourceData, SourceSet,
};

const BASEMAP_Z13: &[u8] = include_bytes!("../../../go/testdata/basemap-13-7277-3227.mvt");
const BASEMAP_Z14: &[u8] = include_bytes!("../../../go/testdata/basemap-14-14554-6454.mvt");
const TERRAIN_Z10: &[u8] = include_bytes!("../../../go/testdata/terrain-10-909-402.webp");
const HILLSHADE: &str = include_str!("../examples/styles/hillshade.json");

/// Land stitched across tile borders and blurred, so what is bound at the
/// neighbours reaches the pixels.
const STITCHED: &str = r##"{
    "name": "test",
    "tile-size": 64,
    "sources": { "basemap": { "type": "mvt", "url": "https://x/{z}/{x}/{y}.mvt" } },
    "nodes": {
        "f": { "op": "features", "source": "basemap", "layer": "water", "stitch": "merge" },
        "fill": { "op": "fill-solid", "features": "@f", "fill": "#204080" },
        "out": { "op": "blur", "input": "@fill", "sigma": 3.0 }
    },
    "output": "out"
}"##;

const IMAGERY: &str = r##"{
    "name": "test",
    "tile-size": 64,
    "pad": 8,
    "sources": { "photo": { "type": "raster", "url": "https://x/{z}/{x}/{y}.webp" } },
    "nodes": {
        "photo": { "op": "raster", "source": "photo" },
        "out": { "op": "blur", "input": "@photo", "sigma": 2.0 }
    },
    "output": "out"
}"##;

const PINS: &str = r##"{
    "name": "test",
    "tile-size": 64,
    "sources": { "pins": { "type": "geojson", "url": "https://x/pins.geojson" } },
    "nodes": {
        "f": { "op": "features", "source": "pins", "layer": "pins" },
        "out": { "op": "fill-solid", "features": "@f", "fill": "#c03020" }
    },
    "output": "out"
}"##;

const PIN_DOC: &str = r#"{
    "type": "FeatureCollection",
    "features": [
        { "type": "Feature", "properties": {},
          "geometry": { "type": "Polygon", "coordinates": [[
              [139.70, 35.60], [139.80, 35.60], [139.80, 35.70],
              [139.70, 35.70], [139.70, 35.60]
          ]] } }
    ]
}"#;

fn rgba() -> RenderOptions {
    RenderOptions {
        format: OutputFormat::Rgba,
        ..RenderOptions::default()
    }
}

fn at(coord: (i32, i32), source_zoom: Option<u8>) -> BindOptions {
    BindOptions {
        coord,
        source_zoom,
        ..BindOptions::default()
    }
}

fn neighbourhood() -> impl Iterator<Item = (i32, i32)> {
    (-1..=1).flat_map(|dy| (-1..=1).map(move |dx| (dx, dy)))
}

/// Bytes to bind at a neighbour offset, with the zoom they are encoded at
/// when that is shallower than the tile's.
type Placed<'a> = ((i32, i32), &'a [u8], Option<u8>);

/// One payload per offset, bound both ways: through `bind_source` on one
/// renderer, and loaded into a set on another.
fn both_ways(
    style: &str,
    source: &str,
    tile: (u8, u32, u32),
    payloads: &[Placed<'_>],
) -> (Vec<u8>, Vec<u8>) {
    let (z, x, y) = tile;
    let mut bound = Renderer::new(style).expect("builds");
    for &(coord, bytes, sz) in payloads {
        bound
            .bind_source(source, bytes.to_vec(), &at(coord, sz))
            .expect("binds");
    }
    let want = bound.render_tile(z, x, y, rgba()).expect("renders");

    let r = Renderer::new(style).expect("builds");
    let mut set = SourceSet::new();
    for &(coord, bytes, sz) in payloads {
        let data = r.load_source(source, bytes).expect("loads");
        set.bind(&data, &at(coord, sz)).expect("binds");
    }
    let got = r.render_tile_with(z, x, y, &set, rgba()).expect("renders");
    (want, got)
}

#[test]
fn vector_tiles_and_their_neighbours_render_as_bound() {
    // The centre at its own zoom, the neighbours overzoomed from the z13
    // parent, so both binding paths of a vector tile are exercised.
    let mut payloads = vec![((0, 0), BASEMAP_Z14, None)];
    payloads.extend(
        neighbourhood()
            .filter(|&c| c != (0, 0))
            .map(|c| (c, BASEMAP_Z13, Some(13))),
    );
    let (want, got) = both_ways(STITCHED, "basemap", (14, 14554, 6454), &payloads);
    assert!(got == want, "a set renders other pixels than bind_source");

    // And the neighbours do reach the pixels, so the comparison above is
    // not only of the centre.
    let (centre_only, _) = both_ways(STITCHED, "basemap", (14, 14554, 6454), &payloads[..1]);
    assert!(centre_only != want, "the neighbours changed nothing");
}

#[test]
fn dem_tiles_render_as_bound() {
    let payloads: Vec<_> = neighbourhood().map(|c| (c, TERRAIN_Z10, None)).collect();
    let (want, got) = both_ways(HILLSHADE, "terrain", (10, 909, 402), &payloads);
    assert!(got == want, "a set renders other pixels than bind_source");

    // Overzoomed: a z11 child drawn from its z10 parent.
    let payloads: Vec<_> = neighbourhood()
        .map(|c| (c, TERRAIN_Z10, Some(10)))
        .collect();
    let (want, got) = both_ways(HILLSHADE, "terrain", (11, 1818, 804), &payloads);
    assert!(got == want, "an overzoomed set renders other pixels");
}

#[test]
fn imagery_tiles_render_as_bound() {
    let payloads: Vec<_> = neighbourhood().map(|c| (c, TERRAIN_Z10, None)).collect();
    let (want, got) = both_ways(IMAGERY, "photo", (10, 909, 402), &payloads);
    assert!(got == want, "a set renders other pixels than bind_source");

    let payloads: Vec<_> = neighbourhood()
        .map(|c| (c, TERRAIN_Z10, Some(10)))
        .collect();
    let (want, got) = both_ways(IMAGERY, "photo", (12, 3637, 1609), &payloads);
    assert!(got == want, "an overzoomed set renders other pixels");
}

#[test]
fn remote_geojson_renders_as_bound() {
    // z10 tile 909/403 holds the whole polygon.
    let payloads = [((0, 0), PIN_DOC.as_bytes(), None)];
    let (want, got) = both_ways(PINS, "pins", (10, 909, 403), &payloads);
    assert!(
        want.chunks(4).any(|p| p[3] > 0),
        "the polygon is in the tile"
    );
    assert!(got == want, "a set renders other pixels than bind_source");
}

/// The renders of `tiles`, one `bind_source` cycle each, every tile
/// binding the same payload at all nine offsets.
fn bound_per_tile(
    style: &str,
    source: &str,
    bytes: &[u8],
    tiles: &[(u8, u32, u32)],
) -> Vec<Vec<u8>> {
    let mut r = Renderer::new(style).expect("builds");
    tiles
        .iter()
        .map(|&(z, x, y)| {
            r.clear_sources();
            for c in neighbourhood() {
                r.bind_source(source, bytes.to_vec(), &at(c, None))
                    .expect("binds");
            }
            r.render_tile(z, x, y, rgba()).expect("renders")
        })
        .collect()
}

#[test]
fn one_loaded_tile_serves_several_sets() {
    let tiles = [(14, 14554, 6454), (14, 14555, 6454)];
    let want = bound_per_tile(STITCHED, "basemap", BASEMAP_Z14, &tiles);

    // Loaded once, bound into both tiles' sets at every offset — as a
    // centre in one, and a neighbour in both.
    let r = Renderer::new(STITCHED).expect("builds");
    let data = r.load_source("basemap", BASEMAP_Z14).expect("loads");
    let sets: Vec<SourceSet> = tiles
        .iter()
        .map(|_| {
            let mut set = SourceSet::new();
            for c in neighbourhood() {
                set.bind(&data, &at(c, None)).expect("binds");
            }
            set
        })
        .collect();
    for ((&(z, x, y), set), want) in tiles.iter().zip(&sets).zip(&want) {
        let got = r.render_tile_with(z, x, y, set, rgba()).expect("renders");
        assert!(
            &got == want,
            "{z}/{x}/{y} renders other pixels from a shared load"
        );
    }
}

#[test]
fn renders_from_sets_can_share_a_renderer_across_threads() {
    fn shareable<T: Send + Sync>() {}
    shareable::<Renderer>();
    shareable::<SourceSet>();
    shareable::<SourceData>();

    let tiles = [
        (10, 908, 401),
        (10, 909, 401),
        (10, 908, 402),
        (10, 909, 402),
    ];
    let want = bound_per_tile(HILLSHADE, "terrain", TERRAIN_Z10, &tiles);

    let r = Renderer::new(HILLSHADE).expect("builds");
    let data = r.load_source("terrain", TERRAIN_Z10).expect("loads");
    let mut set = SourceSet::new();
    for c in neighbourhood() {
        set.bind(&data, &at(c, None)).expect("binds");
    }
    let got: Vec<Vec<u8>> = std::thread::scope(|s| {
        let handles: Vec<_> = tiles
            .iter()
            .map(|&(z, x, y)| {
                let (r, set) = (&r, &set);
                s.spawn(move || r.render_tile_with(z, x, y, set, rgba()).expect("renders"))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("no panic"))
            .collect()
    });
    assert!(
        got == want,
        "concurrent renders differ from sequential ones"
    );
}

/// A vector tile prepares its features for filtering once per zoom, on the
/// first render that reads it at that zoom, and renders on other threads
/// reading the same load wait for that one preparation. Several threads
/// that all start on freshly loaded data draw what renders one at a time
/// draw.
#[test]
fn renders_on_several_threads_share_a_loaded_vector_tile() {
    let tiles = [
        (14, 14554, 6454),
        (14, 14555, 6454),
        (14, 14554, 6455),
        (14, 14555, 6455),
    ];
    let want = bound_per_tile(STITCHED, "basemap", BASEMAP_Z14, &tiles);

    // A few rounds, each on a fresh load, so the first preparation is
    // raced each time.
    for _ in 0..4 {
        let r = Renderer::new(STITCHED).expect("builds");
        let data = r.load_source("basemap", BASEMAP_Z14).expect("loads");
        let sets: Vec<SourceSet> = tiles
            .iter()
            .map(|_| {
                let mut set = SourceSet::new();
                for c in neighbourhood() {
                    set.bind(&data, &at(c, None)).expect("binds");
                }
                set
            })
            .collect();
        // Two threads per tile, held at the line until all are ready.
        let jobs: Vec<usize> = (0..tiles.len()).chain(0..tiles.len()).collect();
        let start = std::sync::Barrier::new(jobs.len());
        let got: Vec<(usize, Vec<u8>)> = std::thread::scope(|s| {
            let handles: Vec<_> = jobs
                .iter()
                .map(|&i| {
                    let (r, sets, start) = (&r, &sets, &start);
                    let (z, x, y) = tiles[i];
                    s.spawn(move || {
                        start.wait();
                        (
                            i,
                            r.render_tile_with(z, x, y, &sets[i], rgba())
                                .expect("renders"),
                        )
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("no panic"))
                .collect()
        });
        for (i, pixels) in got {
            let (z, x, y) = tiles[i];
            assert!(
                pixels == want[i],
                "{z}/{x}/{y} rendered on several threads differs from one at a time"
            );
        }
    }
}

#[test]
fn a_set_ignores_what_is_bound_to_the_renderer() {
    let mut r = Renderer::new(STITCHED).expect("builds");
    let empty = r
        .render_tile_with(14, 14554, 6454, &SourceSet::new(), rgba())
        .expect("renders");
    r.bind_source("basemap", BASEMAP_Z14.to_vec(), &BindOptions::default())
        .expect("binds");
    let still_empty = r
        .render_tile_with(14, 14554, 6454, &SourceSet::new(), rgba())
        .expect("renders");
    assert!(
        empty == still_empty,
        "the bound table leaked into a set's render"
    );
}

#[test]
fn loading_refuses_what_a_set_cannot_hold() {
    let r = Renderer::new(
        r##"{
            "name": "test",
            "sources": {
                "terrain": { "type": "dem", "url": "https://x/{z}/{x}/{y}.png",
                             "encoding": "terrarium" },
                "face": { "type": "font", "url": "file:noto.ttf" }
            },
            "nodes": { "bg": { "op": "solid", "color": "#ffffff" } },
            "output": "bg"
        }"##,
    )
    .expect("builds");
    // A font belongs to the renderer, not to a tile.
    assert_eq!(
        r.load_source("face", b"").unwrap_err().kind(),
        ErrorKind::UnknownSource
    );
    assert_eq!(
        r.load_source("nowhere", b"").unwrap_err().kind(),
        ErrorKind::UnknownSource
    );
    // Unlike `bind_source`, loading decodes a DEM, so rubbish fails here.
    let e = r.load_source("terrain", b"not an image").unwrap_err();
    assert_eq!(e.kind(), ErrorKind::DemDecode);
    assert!(e.to_string().starts_with("source `terrain`:"), "got {e}");
}

#[test]
fn data_loaded_under_another_style_is_refused() {
    let mut r = Renderer::new(HILLSHADE).expect("builds");
    let data = r.load_source("terrain", TERRAIN_Z10).expect("loads");
    let mut set = SourceSet::new();
    set.bind(&data, &BindOptions::default()).expect("binds");
    // Same name, now a raster: the elevations are not pixels.
    r.set_style(
        r##"{
            "name": "test",
            "tile-size": 64,
            "sources": { "terrain": { "type": "raster", "url": "https://x/{z}/{x}/{y}.webp" } },
            "nodes": { "out": { "op": "raster", "source": "terrain" } },
            "output": "out"
        }"##,
    )
    .expect("builds");
    let e = r.render_tile_with(10, 909, 402, &set, rgba()).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::UnknownSource);
}

#[test]
fn a_tile_rendered_from_other_data_is_not_served_from_the_cache() {
    // The same tile from two sets that differ only in what the centre
    // holds. The render cache is keyed by tile and binding name, so
    // without the data in that key the second render would answer with
    // the first one's pixels.
    let tile = (14, 14554, 6454);
    let render_with = |r: &Renderer, bytes: &[u8]| {
        let data = r.load_source("basemap", bytes).expect("loads");
        let mut set = SourceSet::new();
        set.bind(&data, &BindOptions::default()).expect("binds");
        r.render_tile_with(tile.0, tile.1, tile.2, &set, rgba())
            .expect("renders")
    };
    let r = Renderer::new(STITCHED).expect("builds");
    let first = render_with(&r, BASEMAP_Z14);
    let second = render_with(&r, BASEMAP_Z13);
    let fresh = render_with(&Renderer::new(STITCHED).expect("builds"), BASEMAP_Z13);
    assert!(first != fresh, "the two payloads draw the same pixels");
    assert!(
        second == fresh,
        "the second set rendered from the first one's cache"
    );

    // Rebinding other bytes through `bind_source` is the same case.
    let mut r = Renderer::new(STITCHED).expect("builds");
    for bytes in [BASEMAP_Z14, BASEMAP_Z13] {
        r.clear_sources();
        r.bind_source("basemap", bytes.to_vec(), &BindOptions::default())
            .expect("binds");
    }
    let rebound = r
        .render_tile(tile.0, tile.1, tile.2, rgba())
        .expect("renders");
    let mut r2 = Renderer::new(STITCHED).expect("builds");
    r2.bind_source("basemap", BASEMAP_Z14.to_vec(), &BindOptions::default())
        .expect("binds");
    r2.render_tile(tile.0, tile.1, tile.2, rgba())
        .expect("renders");
    r2.clear_sources();
    r2.bind_source("basemap", BASEMAP_Z13.to_vec(), &BindOptions::default())
        .expect("binds");
    let after_rebind = r2
        .render_tile(tile.0, tile.1, tile.2, rgba())
        .expect("renders");
    assert!(after_rebind == rebound, "a rebind rendered from the cache");
}

#[test]
fn a_render_hands_back_the_features_a_node_held() {
    let tile = (14, 14554, 6454);
    let r = Renderer::new(STITCHED).expect("builds");
    let data = r.load_source("basemap", BASEMAP_Z14).expect("loads");
    let mut set = SourceSet::new();
    set.bind(&data, &BindOptions::default()).expect("binds");
    let plain = r
        .render_tile_with(tile.0, tile.1, tile.2, &set, rgba())
        .expect("renders");

    // A fresh renderer, so the values are computed rather than read from
    // the cache — and then again from the cache, which must hand back the
    // same values.
    for r in [Renderer::new(STITCHED).expect("builds"), r] {
        let (image, features) = r
            .render_tile_with_features(tile.0, tile.1, tile.2, &set, rgba(), &["f", "f"])
            .expect("renders");
        assert!(image == plain, "capturing changed the pixels");
        assert_eq!(features.len(), 2, "one entry per name, repeats included");
        let f = features[0].features().expect("the node ran");
        assert_eq!(f.extent, 4096);
        assert!(!f.groups.is_empty(), "the tile has water");
        assert_eq!(features[0].node(), "f");
        let doc: serde_json::Value =
            serde_json::from_str(&features[1].to_geojson()).expect("valid JSON");
        assert_eq!(doc["type"], "FeatureCollection");
        assert_eq!(doc["node"], "f");
        assert_eq!(doc["extent"], 4096);
        assert_eq!(
            doc["features"].as_array().map(Vec::len),
            Some(f.groups.len())
        );
    }

    let refused = |nodes: &[&str]| {
        let r = Renderer::new(STITCHED).expect("builds");
        let e = r
            .render_tile_with_features(tile.0, tile.1, tile.2, &set, rgba(), nodes)
            .unwrap_err();
        assert_eq!(e.kind(), ErrorKind::InvalidStyle);
        e.to_string()
    };
    assert!(refused(&["nope"]).contains("no node `nope`"));
    assert!(refused(&["fill"]).contains("`fill` produces raster"));
}

/// A tile's features in its pixels follow the size the render drew at, and
/// in longitude and latitude land on the tile itself.
#[test]
fn a_tiles_features_write_in_its_pixels_or_on_the_globe() {
    use ezu::renderer::{TileFeatureCoords, TileId};
    let tile = (14, 14554, 6454);
    let r = Renderer::new(STITCHED).expect("builds");
    let data = r.load_source("basemap", BASEMAP_Z14).expect("loads");
    let mut set = SourceSet::new();
    set.bind(&data, &BindOptions::default()).expect("binds");
    let opts = RenderOptions {
        tile_size: Some(200),
        ..rgba()
    };
    let (_, features) = r
        .render_tile_with_features(tile.0, tile.1, tile.2, &set, opts, &["f"])
        .expect("renders");
    let f = &features[0];
    assert_eq!(
        (f.tile(), f.tile_size()),
        (TileId::new(14, 14554, 6454), 200)
    );
    let first = |coords| {
        let doc: serde_json::Value =
            serde_json::from_str(&f.to_geojson_in(coords)).expect("valid JSON");
        let mut g = &doc["features"][0]["geometry"]["coordinates"];
        while g[0].is_array() {
            g = &g[0];
        }
        (g[0].as_f64().unwrap(), g[1].as_f64().unwrap())
    };
    let (u, v) = first(TileFeatureCoords::Tile);
    let (px, py) = first(TileFeatureCoords::Px);
    let scale = 200.0 / f.features().unwrap().extent as f64;
    assert!((px - u * scale).abs() < 0.006 && (py - v * scale).abs() < 0.006);
    let (lon, lat) = first(TileFeatureCoords::LonLat);
    let n = (1u32 << tile.0) as f64;
    let x = ezu::core::coord::lon_to_world_x(lon) * n - tile.1 as f64;
    let y = ezu::core::coord::lat_to_world_y(lat) * n - tile.2 as f64;
    assert!(
        (x * 200.0 - px).abs() < 0.01 && (y * 200.0 - py).abs() < 0.01,
        "{lon},{lat}"
    );
}

#[test]
fn an_observed_render_shows_every_node_and_draws_the_same_tile() {
    use std::sync::Mutex;
    struct Seen(Mutex<Vec<String>>);
    impl ezu::graph::NodeObserver for Seen {
        fn on_node(&self, _: ezu::graph::NodeIx, value: &ezu::graph::PortValue, _: bool, _: u128) {
            self.0.lock().unwrap().push(format!("{:?}", value.kind()));
        }
    }
    let tile = (14, 14554, 6454);
    let r = Renderer::new(STITCHED).expect("builds");
    let mut set = SourceSet::new();
    set.bind(
        &r.load_source("basemap", BASEMAP_Z14).expect("loads"),
        &BindOptions::default(),
    )
    .expect("binds");
    let plain = r
        .render_tile_with(tile.0, tile.1, tile.2, &set, rgba())
        .expect("renders");
    let seen = Seen(Mutex::new(Vec::new()));
    let observed = r
        .render_tile_observed(tile.0, tile.1, tile.2, &set, rgba(), &seen)
        .expect("renders");
    assert!(observed == plain);
    // `f`, `fill` and `out`: every node, cached or not.
    assert_eq!(seen.0.lock().unwrap().len(), 3);
}

/// Labels on a geojson source, with the document either loaded into the
/// set or carried inline as `data`.
fn geojson_labels(data: Option<&str>) -> String {
    let source = match data {
        Some(doc) => format!(r#"{{ "type": "geojson", "data": {doc} }}"#),
        None => r#"{ "type": "geojson", "url": "https://example.invalid/names.geojson" }"#.into(),
    };
    format!(
        r##"{{
        "name": "names", "tile-size": 64,
        "sources": {{
            "g": {{ "type": "glyphs", "url": "https://example.invalid/{{fontstack}}/{{range}}.pbf", "fontstack": "Sans" }},
            "names": {source}
        }},
        "nodes": {{
            "f": {{ "op": "features", "source": "names", "layer": "names" }},
            "l": {{ "op": "text-labels", "features": "@f", "source": "names", "layer": "names",
                    "font": ["g"], "text": ["get", "name"], "size": 16, "color": "#000000" }},
            "p": {{ "op": "label-placement", "labels": ["@l"] }},
            "out": {{ "op": "text-draw", "labels": "@l", "placement": "@p" }}
        }},
        "output": "@out"
    }}"##
    )
}

const NAMES_DOC: &str = r#"{ "type": "FeatureCollection", "features": [
    { "type": "Feature", "properties": { "name": "重" },
      "geometry": { "type": "Point", "coordinates": [139.77, 35.68] } } ] }"#;

#[test]
fn the_glyphs_a_set_needs_include_its_geojson_labels() {
    let r = Renderer::new(&geojson_labels(None)).expect("builds");
    let data = r.load_source("names", NAMES_DOC.as_bytes()).expect("loads");
    let mut set = SourceSet::new();
    set.bind(&data, &BindOptions::default()).expect("binds");

    let codepoints = r.needed_codepoints_in(&set);
    assert!(codepoints["g"].contains(&0x91CD), "{codepoints:?}");
    let ranges = r.needed_glyph_ranges_in(&set);
    assert!(ranges["g"].contains(&0x9100), "{ranges:?}");
}

#[test]
fn the_glyphs_a_style_needs_include_its_inline_geojson_labels() {
    let r = Renderer::new(&geojson_labels(Some(NAMES_DOC))).expect("builds");
    let codepoints = r.needed_codepoints_in(&SourceSet::new());
    assert!(codepoints["g"].contains(&0x91CD), "{codepoints:?}");
    let bound = r.needed_codepoints().expect("lists");
    assert!(bound["g"].contains(&0x91CD), "{bound:?}");
}
