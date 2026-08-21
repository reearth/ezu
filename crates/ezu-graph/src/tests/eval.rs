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
            vec![PortSpec::new("input", &[PortKind::Raster])],
            PortKind::Raster,
        )
        .boxed()
    };
    let merge = Mock::new(
        "merge",
        vec![
            PortSpec::new("left", &[PortKind::Raster]),
            PortSpec::new("right", &[PortKind::Raster]),
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

/// Produces a raster and hands the test a `Weak` to it, so the test can
/// ask later whether the evaluator is still holding it.
struct Watched {
    weak: std::sync::Mutex<Option<std::sync::Weak<crate::RasterBuf>>>,
}

impl Node for Watched {
    fn op_name(&self) -> &'static str {
        "watched"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self, _: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
    }
    fn eval(
        &self,
        ctx: &crate::EvalCtx<'_>,
        _inputs: &[Option<crate::PortValue>],
    ) -> Result<crate::PortValue, crate::EvalError> {
        let (pw, ph) = ctx.canvas.padded_dims();
        let buf = Arc::new(crate::RasterBuf::filled(pw, ph, [1, 2, 3, 255]));
        *self.weak.lock().unwrap() = Some(Arc::downgrade(&buf));
        Ok(crate::PortValue::Raster(buf))
    }
    fn param_hash(&self, h: &mut xxhash_rust::xxh3::Xxh3) {
        h.update(b"watched");
    }
}

/// Records, at its own eval time, how many strong references the
/// `Watched` buffer still has.
struct Observer {
    weak: Arc<Watched>,
    seen: std::sync::Mutex<Option<usize>>,
    ports: Vec<PortSpec>,
}

impl Node for Observer {
    fn op_name(&self) -> &'static str {
        "observer"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
    }
    fn eval(
        &self,
        _ctx: &crate::EvalCtx<'_>,
        inputs: &[Option<crate::PortValue>],
    ) -> Result<crate::PortValue, crate::EvalError> {
        let strong = self
            .weak
            .weak
            .lock()
            .unwrap()
            .as_ref()
            .map_or(0, std::sync::Weak::strong_count);
        *self.seen.lock().unwrap() = Some(strong);
        Ok(inputs[0].clone().unwrap())
    }
    fn param_hash(&self, h: &mut xxhash_rust::xxh3::Xxh3) {
        h.update(b"observer");
    }
}

#[test]
fn intermediates_are_released_once_their_consumers_have_run() {
    // watched -> mid -> observer. By the time `observer` runs, nothing
    // should still be holding `watched`'s raster: `mid` was its only
    // consumer and has already produced its own output.
    let watched = Arc::new(Watched {
        weak: std::sync::Mutex::new(None),
    });
    let observer = Arc::new(Observer {
        weak: Arc::clone(&watched),
        seen: std::sync::Mutex::new(None),
        ports: vec![PortSpec::new("input", &[PortKind::Raster])],
    });

    let mut b = GraphBuilder::new();
    b.add_node(
        "watched",
        Box::new(Forward(watched.clone() as Arc<dyn Node>)),
    )
    .add_node(
        "mid",
        Mock::new(
            "mid",
            vec![PortSpec::new("input", &[PortKind::Raster])],
            PortKind::Raster,
        )
        .boxed(),
    )
    .add_node(
        "observer",
        Box::new(Forward(observer.clone() as Arc<dyn Node>)),
    )
    .connect("watched", "mid", "input")
    .connect("mid", "observer", "input")
    .set_output("observer");
    let g = b.build().unwrap();
    // Byte budget 0 so the cache is not the thing keeping it alive.
    let cache = Cache::with_limits(64, 0);
    let assets = NoAssets;
    let ev = Evaluator::new(&g, &cache, &assets);
    ev.render(
        TileId { z: 0, x: 0, y: 0 },
        small_canvas(),
        &ParamValues::new(),
        0,
    )
    .unwrap();
    assert_eq!(
        *observer.seen.lock().unwrap(),
        Some(0),
        "an intermediate should be dropped once its last consumer has run"
    );
}

#[test]
fn blank_rasters_share_one_buffer() {
    // Two independent transparent sources feeding one node: both inputs
    // should arrive as the same interned allocation.
    struct SamePtr {
        ports: Vec<PortSpec>,
        same: std::sync::atomic::AtomicBool,
    }
    impl Node for SamePtr {
        fn op_name(&self) -> &'static str {
            "same-ptr"
        }
        fn inputs(&self) -> &[PortSpec] {
            &self.ports
        }
        fn output(&self, _: &[Option<PortKind>]) -> PortKind {
            PortKind::Raster
        }
        fn eval(
            &self,
            _ctx: &crate::EvalCtx<'_>,
            inputs: &[Option<crate::PortValue>],
        ) -> Result<crate::PortValue, crate::EvalError> {
            let a = inputs[0].as_ref().unwrap().as_raster().unwrap();
            let b = inputs[1].as_ref().unwrap().as_raster().unwrap();
            self.same
                .store(Arc::ptr_eq(a, b), std::sync::atomic::Ordering::SeqCst);
            Ok(inputs[0].clone().unwrap())
        }
        fn param_hash(&self, h: &mut xxhash_rust::xxh3::Xxh3) {
            h.update(b"same-ptr");
        }
    }

    let probe = Arc::new(SamePtr {
        ports: vec![
            PortSpec::new("left", &[PortKind::Raster]),
            PortSpec::new("right", &[PortKind::Raster]),
        ],
        same: std::sync::atomic::AtomicBool::new(false),
    });
    let mut b = GraphBuilder::new();
    b.add_node(
        "l",
        Mock::new("l", vec![], PortKind::Raster)
            .transparent()
            .boxed(),
    )
    .add_node(
        "r",
        Mock::new("r", vec![], PortKind::Raster)
            .transparent()
            .boxed(),
    )
    .add_node("probe", Box::new(Forward(probe.clone() as Arc<dyn Node>)))
    .connect("l", "probe", "left")
    .connect("r", "probe", "right")
    .set_output("probe");
    let g = b.build().unwrap();
    let cache = Cache::with_limits(64, 0);
    let assets = NoAssets;
    Evaluator::new(&g, &cache, &assets)
        .render(
            TileId { z: 0, x: 0, y: 0 },
            small_canvas(),
            &ParamValues::new(),
            0,
        )
        .unwrap();
    assert!(
        probe.same.load(std::sync::atomic::Ordering::SeqCst),
        "blank rasters should collapse onto one shared buffer"
    );
}

#[test]
fn cache_evicts_to_stay_within_its_byte_budget() {
    let px =
        |n: u8| crate::PortValue::Raster(Arc::new(crate::RasterBuf::filled(8, 8, [n, 0, 0, 255])));
    let one = px(1).approx_bytes();
    // Room for two rasters, not three.
    let cache = Cache::with_limits(64, one * 2);
    let keys: Vec<crate::CacheKey> = (0..3).map(|i| crate::CacheKey(i as u128)).collect();
    for (i, k) in keys.iter().enumerate() {
        cache.insert(*k, px(i as u8));
    }
    assert!(
        cache.bytes() <= one * 2,
        "budget should bound retained bytes"
    );
    assert!(cache.get(keys[2]).is_some(), "newest entry is kept");
    assert!(
        cache.get(keys[0]).is_none(),
        "oldest entry is evicted first"
    );
}
