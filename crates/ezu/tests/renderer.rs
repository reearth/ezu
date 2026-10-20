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

/// A native host can let the renderer read the style's assets itself: a
/// brush beside the style draws what binding its bytes draws, and a glyphs
/// source relative to the style reads each range as a label needs it.
#[cfg(feature = "http")]
#[tokio::test]
async fn resolved_assets_draw_what_bound_ones_draw() {
    use ezu::renderer::{BindOptions, OutputFormat, RenderOptions, SourceSet};
    use std::path::Path;

    let styles = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/styles");
    let watercolor = std::fs::read_to_string(styles.join("watercolor.json")).expect("reads");
    let tile = include_bytes!("../../../go/testdata/basemap-14-14554-6454.mvt");
    let rgba = || RenderOptions {
        format: OutputFormat::Rgba,
        tile_size: Some(128),
        ..RenderOptions::default()
    };
    let render = |r: &Renderer| {
        let mut set = SourceSet::new();
        set.bind(
            &r.load_source("basemap", tile).expect("loads"),
            &BindOptions::default(),
        )
        .expect("binds");
        r.render_tile_with(14, 14554, 6454, &set, rgba())
            .expect("renders")
    };

    let mut bound = Renderer::new(&watercolor).expect("builds");
    let brush = std::fs::read(styles.join("brushes/watercolor_glazing.myb")).expect("reads");
    bound
        .bind_source("glazing", brush, &BindOptions::default())
        .expect("binds");
    let mut resolved = Renderer::new(&watercolor).expect("builds");
    resolved.resolve_assets(&styles).await.expect("resolves");
    assert!(render(&resolved) == render(&bound));

    // Glyphs: a `file:` template relative to the style, which only the
    // resolved renderer can read.
    let dir = std::env::temp_dir().join(format!("ezu-resolve-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("glyphs/Sans")).expect("a dir");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../ezu-core/tests/glyphs/0-255.pbf"),
        dir.join("glyphs/Sans/0-255.pbf"),
    )
    .expect("copies");
    let labels = r##"{
        "name": "labels", "tile-size": 64,
        "sources": { "g": { "type": "glyphs", "url": "file:glyphs/{fontstack}/{range}.pbf", "fontstack": "Sans" } },
        "nodes": {
            "pt": { "op": "literal-geometry", "points": [[2048, 2048]] },
            "l": { "op": "text-labels", "features": "@pt", "font": ["g"], "text": "Ab", "size": 16, "color": "#000000" },
            "p": { "op": "label-placement", "labels": ["@l"] },
            "out": { "op": "text-draw", "labels": "@l", "placement": "@p" }
        },
        "output": "@out"
    }"##;
    let mut r = Renderer::new(labels).expect("builds");
    r.resolve_assets(&dir).await.expect("resolves");
    let drawn = r
        .render_tile_with(1, 0, 0, &SourceSet::new(), rgba())
        .expect("renders");
    assert_eq!(r.memory_usage().glyph_ranges, 1, "the range was read");
    assert!(drawn.chunks(4).any(|p| p[3] > 0), "the label drew");
    std::fs::remove_dir_all(&dir).ok();
}

/// A string param is overridden through `RenderOptions::params` the way a
/// number is, its `enum` checked there, and its declaration published in
/// the params schema the host builds controls from.
#[test]
fn a_string_param_is_overridden_and_checked_per_render() {
    use ezu::renderer::{ErrorKind, OutputFormat, RenderOptions};

    let style = r##"{
      "name": "modes",
      "tile-size": 16,
      "params": {
        "mode": { "type": "string", "enum": ["multiply", "screen"], "default": "multiply" }
      },
      "nodes": {
        "base": { "op": "solid", "color": "#808080" },
        "over": { "op": "solid", "color": "#ff4020" },
        "out":  { "op": "blend", "base": "@base", "over": "@over", "mode": "$mode" }
      },
      "output": "@out"
    }"##;
    let r = Renderer::new(style).expect("builds");
    assert_eq!(
        r.params_schema()["properties"]["mode"]["enum"],
        serde_json::json!(["multiply", "screen"])
    );
    let render = |mode: Option<&str>| {
        r.render_tile(
            0,
            0,
            0,
            RenderOptions {
                format: OutputFormat::Rgba,
                params: mode
                    .map(|m| vec![("mode".to_string(), m.to_string())])
                    .unwrap_or_default(),
                ..RenderOptions::default()
            },
        )
    };
    let plain = render(None).expect("renders");
    assert_eq!(render(Some("multiply")).expect("renders"), plain);
    assert_ne!(render(Some("screen")).expect("renders"), plain);
    let err = render(Some("overlay")).expect_err("not in the enum");
    assert_eq!(err.kind(), ErrorKind::InvalidStyle);
}
