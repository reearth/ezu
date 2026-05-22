//! Evaluator + cache tests. Use a counting node to verify that shared
//! upstreams evaluate once per render and cached intermediates survive
//! across renders for the same tile.

use std::sync::Arc;

use super::common::{small_canvas, Counter, Forward, Mock};
use crate::{
    Cache, Evaluator, GraphBuilder, NoAssets, Node, ParamValues, PortKind, PortSpec, TileId,
};

#[test]
fn evaluator_returns_output_value() {
    let mut b = GraphBuilder::new();
    b.add_node(
        "a",
        Box::new(Forward(Counter::new("src", PortKind::Raster))),
    )
    .set_output("a");
    let g = b.build().unwrap();
    let cache = Cache::new();
    let assets = NoAssets;
    let ev = Evaluator::new(&g, &cache, &assets);
    let out = ev
        .render(
            TileId { z: 0, x: 0, y: 0 },
            small_canvas(),
            &ParamValues::new(),
            0,
        )
        .unwrap();
    let raster = out.as_raster().unwrap();
    assert_eq!(raster.width, 8);
    assert_eq!(raster.pixel(0, 0), [64, 0, 0, 64]);
}

#[test]
fn evaluator_evaluates_each_node_once_per_render() {
    // diamond: a -> b, a -> c, b+c -> d. `a` should eval ONCE.
    let counter = Counter::new("src", PortKind::Raster);
    let pass = |op: &'static str| {
        Mock::new(
            op,
            vec![PortSpec::new("input", PortKind::Raster)],
            PortKind::Raster,
        )
        .boxed()
    };
    let merge = Mock::new(
        "merge",
        vec![
            PortSpec::new("left", PortKind::Raster),
            PortSpec::new("right", PortKind::Raster),
        ],
        PortKind::Raster,
    )
    .boxed();

    let mut b = GraphBuilder::new();
    b.add_node(
        "a",
        Box::new(Forward(Arc::clone(&counter) as Arc<dyn Node>)),
    )
    .add_node("b", pass("b"))
    .add_node("c", pass("c"))
    .add_node("d", merge)
    .connect("a", "b", "input")
    .connect("a", "c", "input")
    .connect("b", "d", "left")
    .connect("c", "d", "right")
    .set_output("d");
    let g = b.build().unwrap();
    let cache = Cache::new();
    let assets = NoAssets;
    let ev = Evaluator::new(&g, &cache, &assets);
    ev.render(
        TileId { z: 0, x: 0, y: 0 },
        small_canvas(),
        &ParamValues::new(),
        0,
    )
    .unwrap();
    assert_eq!(counter.count(), 1, "shared upstream should eval once");
}

#[test]
fn cache_reuses_results_across_renders() {
    let counter = Counter::new("src", PortKind::Raster);
    let mut b = GraphBuilder::new();
    b.add_node(
        "a",
        Box::new(Forward(Arc::clone(&counter) as Arc<dyn Node>)),
    )
    .set_output("a");
    let g = b.build().unwrap();
    let cache = Cache::new();
    let assets = NoAssets;
    let ev = Evaluator::new(&g, &cache, &assets);
    let tile = TileId { z: 0, x: 0, y: 0 };
    let cv = small_canvas();
    ev.render(tile, cv, &ParamValues::new(), 0).unwrap();
    ev.render(tile, cv, &ParamValues::new(), 0).unwrap();
    assert_eq!(counter.count(), 1, "second render should hit cache");
    // Different tile -> miss again.
    ev.render(TileId { z: 0, x: 1, y: 0 }, cv, &ParamValues::new(), 0)
        .unwrap();
    assert_eq!(counter.count(), 2);
}
