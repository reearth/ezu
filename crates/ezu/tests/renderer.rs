//! The embedding renderer is reachable through the umbrella crate, and
//! answers there what the JS and Go packages answer for the same frame.

#![cfg(feature = "renderer")]

use ezu::renderer::{FrameRequest, Renderer};

const STAINED_GLASS: &str = include_str!("../examples/styles/stained-glass.json");

/// A 245 × 174 mm page at 1:1000 and 200 dpi over Tokyo: the CLI, the Go
/// package and the JS package all render it at z=19 with 489 px tiles.
#[test]
fn a_print_frame_computes_as_the_other_bindings_do() {
    let r = Renderer::new(STAINED_GLASS).expect("builds");
    let f = r
        .compute_frame(&FrameRequest {
            center_lon: 139.70,
            center_lat: 35.68,
            scale: 1000.0,
            dpi: 200.0,
            width: 1929,
            height: 1370,
            zoom: None,
            style_tile_size: r.tile_size(),
        })
        .expect("a frame");
    assert_eq!((f.zoom, f.tile_size), (19, 489));
    assert!((f.scale - 999.8).abs() <= 0.05, "scale 1:{}", f.scale);
    assert_eq!((f.tiles_x, f.tiles_y), (465594..465599, 206453..206457));
}

/// The cache budget is the host's to set, and outlives a style or tile size
/// change, which start a fresh cache of the same size.
#[test]
fn the_cache_budget_is_the_hosts_and_survives_a_new_cache() {
    let mut r = Renderer::new(STAINED_GLASS).expect("builds");
    let default = r.memory_usage().cache_budget;
    assert_eq!(default, 8 * 1024 * 1024);
    r.set_cache_budget(Some(64 * 1024 * 1024));
    assert_eq!(r.memory_usage().cache_budget, 64 * 1024 * 1024);
    r.set_tile_size(Some(256)).expect("a size");
    r.set_style(STAINED_GLASS).expect("builds");
    assert_eq!(r.memory_usage().cache_budget, 64 * 1024 * 1024);
    r.set_cache_budget(None);
    assert_eq!(r.memory_usage().cache_budget, default);
}
