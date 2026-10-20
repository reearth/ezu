//! How much a block of tiles costs when every tile binds its 3×3
//! neighbourhood. Run with
//! `cargo test --release -p ezu --test source_set_bench -- --ignored --nocapture`.

#![cfg(feature = "renderer")]

use std::collections::HashMap;
use std::time::Instant;

use ezu::renderer::{BindOptions, OutputFormat, RenderOptions, Renderer, SourceData, SourceSet};

const BASEMAP_Z14: &[u8] = include_bytes!("../../../go/testdata/basemap-14-14554-6454.mvt");
const TERRAIN_Z10: &[u8] = include_bytes!("../../../go/testdata/terrain-10-909-402.webp");
const HILLSHADE: &str = include_str!("../examples/styles/hillshade.json");

const VECTOR: &str = r##"{
    "name": "bench",
    "tile-size": 256,
    "sources": {
        "basemap": { "type": "mvt", "url": "https://x/{z}/{x}/{y}.mvt" }
    },
    "nodes": {
        "bg": { "op": "solid", "color": "#ffffff" },
        "earth": { "op": "features", "source": "basemap", "layer": "earth" },
        "water": { "op": "features", "source": "basemap", "layer": "water" },
        "buildings": { "op": "features", "source": "basemap", "layer": "buildings" },
        "land": { "op": "fill-solid", "features": "@earth", "fill": "#e8e4d8" },
        "sea": { "op": "fill-solid", "features": "@water", "fill": "#9bc8e5" },
        "house": { "op": "fill-solid", "features": "@buildings", "fill": "#b0a898" },
        "out": { "op": "stack", "layers": ["@bg", "@land", "@sea", "@house"] }
    },
    "output": "@out"
}"##;

/// [`VECTOR`] with every layer stitched across tile borders, so each render
/// reads its eight neighbours' layers as well as its own.
const VECTOR_STITCHED: &str = r##"{
    "name": "bench",
    "tile-size": 256,
    "sources": {
        "basemap": { "type": "mvt", "url": "https://x/{z}/{x}/{y}.mvt" }
    },
    "nodes": {
        "bg": { "op": "solid", "color": "#ffffff" },
        "earth": { "op": "features", "source": "basemap", "layer": "earth", "stitch": "pieces" },
        "water": { "op": "features", "source": "basemap", "layer": "water", "stitch": "pieces" },
        "buildings": { "op": "features", "source": "basemap", "layer": "buildings", "stitch": "pieces" },
        "land": { "op": "fill-solid", "features": "@earth", "fill": "#e8e4d8" },
        "sea": { "op": "fill-solid", "features": "@water", "fill": "#9bc8e5" },
        "house": { "op": "fill-solid", "features": "@buildings", "fill": "#b0a898" },
        "out": { "op": "stack", "layers": ["@bg", "@land", "@sea", "@house"] }
    },
    "output": "@out"
}"##;

const N: u32 = 4;
const ROUNDS: usize = 3;

fn opts() -> RenderOptions {
    RenderOptions {
        format: OutputFormat::Rgba,
        ..RenderOptions::default()
    }
}

/// Every tile of an `N`×`N` block at `z/x0/y0`, binding the same payload at
/// all nine offsets, through `bind_source` and `clear_sources`.
fn bind_per_tile(style: &str, source: &str, bytes: &[u8], z: u8, x0: u32, y0: u32) -> f64 {
    let mut total = 0.0;
    for _ in 0..ROUNDS {
        // A fresh renderer per round, so no round renders from the cache
        // an earlier one filled.
        let mut r = Renderer::new(style).expect("builds");
        let start = Instant::now();
        for ty in 0..N {
            for tx in 0..N {
                r.clear_sources();
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let o = BindOptions {
                            coord: (dx, dy),
                            ..BindOptions::default()
                        };
                        r.bind_source(source, bytes.to_vec(), &o).expect("binds");
                    }
                }
                r.render_tile(z, x0 + tx, y0 + ty, opts()).expect("renders");
            }
        }
        total += start.elapsed().as_secs_f64();
    }
    total * 1000.0 / ROUNDS as f64
}

/// The same block, loading each of the `(N + 2)`² tiles it touches once and
/// binding it into the set of every tile it borders. `threads` renders the
/// block's tiles that many at a time over one renderer.
fn load_once(
    style: &str,
    source: &str,
    bytes: &[u8],
    z: u8,
    x0: u32,
    y0: u32,
    threads: usize,
) -> f64 {
    let mut total = 0.0;
    for _ in 0..ROUNDS {
        let r = Renderer::new(style).expect("builds");
        let start = Instant::now();
        // Every tile the block and its rim cover, keyed by its offset from
        // the block's corner; each would be a separate fetch.
        let span = N as i32 + 2;
        let loaded: HashMap<(i32, i32), SourceData> = (0..span * span)
            .map(|i| {
                let key = (i % span - 1, i / span - 1);
                (key, r.load_source(source, bytes).expect("loads"))
            })
            .collect();
        let jobs: Vec<(u32, u32, SourceSet)> = (0..N * N)
            .map(|i| {
                let (tx, ty) = (i % N, i / N);
                let mut set = SourceSet::new();
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let data = &loaded[&(tx as i32 + dx, ty as i32 + dy)];
                        let o = BindOptions {
                            coord: (dx, dy),
                            ..BindOptions::default()
                        };
                        set.bind(data, &o).expect("binds");
                    }
                }
                (x0 + tx, y0 + ty, set)
            })
            .collect();
        std::thread::scope(|s| {
            for chunk in jobs.chunks(jobs.len().div_ceil(threads)) {
                let r = &r;
                s.spawn(move || {
                    for (x, y, set) in chunk {
                        r.render_tile_with(z, *x, *y, set, opts()).expect("renders");
                    }
                });
            }
        });
        total += start.elapsed().as_secs_f64();
    }
    total * 1000.0 / ROUNDS as f64
}

/// What one decode costs, to read the totals against.
fn one_load(style: &str, source: &str, bytes: &[u8]) -> f64 {
    let r = Renderer::new(style).expect("builds");
    let start = Instant::now();
    for _ in 0..20 {
        r.load_source(source, bytes).expect("loads");
    }
    start.elapsed().as_secs_f64() * 1000.0 / 20.0
}

#[test]
#[ignore = "a measurement, not a check"]
fn a_block_of_tiles_with_their_neighbourhoods() {
    for (label, style, source, bytes, z, x0, y0) in [
        ("mvt", VECTOR, "basemap", BASEMAP_Z14, 14, 14553, 6453),
        (
            "mvt stitched",
            VECTOR_STITCHED,
            "basemap",
            BASEMAP_Z14,
            14,
            14553,
            6453,
        ),
        ("dem", HILLSHADE, "terrain", TERRAIN_Z10, 10, 908, 401),
    ] {
        let decode = one_load(style, source, bytes);
        let bound = bind_per_tile(style, source, bytes, z, x0, y0);
        let once = load_once(style, source, bytes, z, x0, y0, 1);
        let par = load_once(style, source, bytes, z, x0, y0, 4);
        println!("{label} one decode: {decode:.2} ms");
        println!("{label} {N}x{N} via bind_source:          {bound:.1} ms");
        println!("{label} {N}x{N} via load_source, 1 thread:  {once:.1} ms");
        println!("{label} {N}x{N} via load_source, 4 threads: {par:.1} ms");
    }
}
