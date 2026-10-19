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
