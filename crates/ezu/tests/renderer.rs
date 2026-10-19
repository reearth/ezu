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
