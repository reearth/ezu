//! Walk the DAG and evaluate one tile.

#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

use xxhash_rust::xxh3::Xxh3;

use crate::cache::{Cache, CacheKey, Hash128};
use crate::eval::{AssetLoader, CanvasInfo, EvalCtx, EvalError, ParamValues, TileId};
use crate::graph::{Graph, NodeIx};
use crate::port::CoordSpace;
use crate::value::PortValue;

/// Entry point: evaluate a `Graph` for one tile.
pub struct Evaluator<'a> {
    pub graph: &'a Graph,
    pub cache: &'a Cache,
    pub assets: &'a dyn AssetLoader,
}

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error(transparent)]
    Eval(#[from] EvalError),
}

impl<'a> Evaluator<'a> {
    pub fn new(graph: &'a Graph, cache: &'a Cache, assets: &'a dyn AssetLoader) -> Self {
        Self {
            graph,
            cache,
            assets,
        }
    }

    /// Evaluate the graph and return the value at the output node.
    /// Source nodes pull host data through `self.assets`; tile-scoped
    /// bindings (MVT/GeoJSON layers, …) live under `tile.<name>` keys.
    pub fn render(
        &self,
        tile: TileId,
        canvas: CanvasInfo,
        params: &ParamValues,
        rng_seed: u64,
    ) -> Result<PortValue, RenderError> {
        let ctx = EvalCtx {
            tile,
            canvas,
            assets: self.assets,
            params,
            rng_seed,
        };
        let n = self.graph.len();
        let mut hashes: Vec<Hash128> = vec![0; n];
        let mut values: Vec<Option<PortValue>> = vec![None; n];

        for &ix in self.graph.topo_order() {
            let (value, hash) = {
                let upstream = |src: NodeIx| -> (Hash128, PortValue) {
                    (
                        hashes[src],
                        values[src]
                            .clone()
                            .expect("upstream evaluated earlier in topo order"),
                    )
                };
                self.eval_one(ix, &ctx, &upstream)?
            };
            hashes[ix] = hash;
            values[ix] = Some(value);
        }
        Ok(values[self.graph.output()].clone().expect("output unset"))
    }

    /// Like [`render`] but evaluates nodes concurrently on Rayon, firing
    /// each node the moment its last input resolves rather than waiting
    /// on a topological-level barrier. A slow node (e.g. text) no longer
    /// stalls unrelated branches, so the wall time tracks the graph's
    /// critical path instead of the sum of per-level maxima.
    ///
    /// Falls back to sequential evaluation transparently when the
    /// `parallel` feature is disabled, so callers don't need to branch.
    pub fn render_parallel(
        &self,
        tile: TileId,
        canvas: CanvasInfo,
        params: &ParamValues,
        rng_seed: u64,
    ) -> Result<PortValue, RenderError> {
        #[cfg(not(feature = "parallel"))]
        {
            self.render(tile, canvas, params, rng_seed)
        }
        #[cfg(feature = "parallel")]
        {
            use std::sync::atomic::AtomicUsize;
            use std::sync::{Mutex, OnceLock};

            let ctx = EvalCtx {
                tile,
                canvas,
                assets: self.assets,
                params,
                rng_seed,
            };
            let n = self.graph.len();
            let state = ParState {
                slots: (0..n).map(|_| OnceLock::new()).collect(),
                pending: (0..n)
                    .map(|ix| AtomicUsize::new(self.graph.indegree(ix)))
                    .collect(),
                first_err: Mutex::new(None),
                ctx,
            };

            // Bind a reference first: a `move` closure that mentions
            // `state` would otherwise capture it by value.
            let state = &state;
            rayon::scope(|scope| {
                for ix in 0..n {
                    if self.graph.indegree(ix) == 0 {
                        scope.spawn(move |s| self.schedule(s, state, ix));
                    }
                }
            });

            if let Some(e) = state
                .first_err
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
            {
                return Err(e);
            }
            Ok(state.slots[self.graph.output()]
                .get()
                .expect("output unset")
                .0
                .clone())
        }
    }

    /// Evaluate one node and, on success, release its downstream nodes:
    /// each dependent's pending-input count is decremented, and the node
    /// that drives a count to zero spawns that dependent on the same
    /// Rayon scope. On error the first failure is recorded and no further
    /// nodes are released, draining the scope so the caller can surface it.
    #[cfg(feature = "parallel")]
    fn schedule<'scope>(
        &'scope self,
        scope: &rayon::Scope<'scope>,
        state: &'scope ParState<'scope>,
        ix: NodeIx,
    ) {
        use std::sync::atomic::Ordering;

        if state
            .first_err
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
        {
            return;
        }

        let upstream = |src: NodeIx| -> (Hash128, PortValue) {
            let (v, h) = state.slots[src]
                .get()
                .expect("upstream resolved before dependent is scheduled");
            (*h, v.clone())
        };

        match self.eval_one(ix, &state.ctx, &upstream) {
            Ok((v, h)) => {
                let _ = state.slots[ix].set((v, h));
            }
            Err(e) => {
                let mut slot = state.first_err.lock().unwrap_or_else(|p| p.into_inner());
                if slot.is_none() {
                    *slot = Some(e);
                }
                return;
            }
        }

        for &dst in self.graph.downstream_unique(ix) {
            if state.pending[dst].fetch_sub(1, Ordering::AcqRel) == 1 {
                scope.spawn(move |s| self.schedule(s, state, dst));
            }
        }
    }

    /// Evaluate one node given the current intermediate state. Pulled
    /// out so the serial and parallel paths share the cache lookup and
    /// hashing logic; the paths differ only in how upstream results are
    /// fetched (`upstream(src) -> (input hash, input value)`).
    fn eval_one(
        &self,
        ix: NodeIx,
        ctx: &EvalCtx<'_>,
        upstream: &dyn Fn(NodeIx) -> (Hash128, PortValue),
    ) -> Result<(PortValue, Hash128), RenderError> {
        let node = self.graph.node(ix);

        // Hash this node's own params, plus any asset bindings it samples.
        let mut h = Xxh3::new();
        node.param_hash(&mut h);
        for name in node.asset_inputs() {
            h.update(name.as_bytes());
            h.update(&ctx.assets.hash(&name).to_le_bytes());
        }
        // Runtime values of `$param` references read at eval time —
        // overriding a param invalidates exactly the nodes that read it.
        for name in node.param_refs() {
            h.update(name.as_bytes());
            match ctx.params.get(&name) {
                Some(v) => v.hash_into(&mut h),
                None => h.update(b"\0default"),
            }
        }
        let params_hash: Hash128 = h.digest128();

        // Collect input hashes (in port order) and input values.
        let input_specs = node.inputs();
        let mut input_hashes: Vec<Hash128> = Vec::with_capacity(input_specs.len());
        let mut input_vals: Vec<Option<PortValue>> = Vec::with_capacity(input_specs.len());
        for port_ix in 0..input_specs.len() {
            match self.graph.incoming(ix, port_ix) {
                Some(src) => {
                    let (h, v) = upstream(src);
                    input_hashes.push(h);
                    input_vals.push(Some(v));
                }
                None => {
                    input_hashes.push(0);
                    input_vals.push(None);
                }
            }
        }

        // World-anchored nodes drop the tile id from their key so
        // adjacent tiles can share intermediates.
        let tile_for_key = match node.coord_space() {
            CoordSpace::World => None,
            _ => Some(ctx.tile),
        };
        let key = CacheKey::build(ctx.canvas, tile_for_key, params_hash, &input_hashes);

        if let Some(v) = self.cache.get(key) {
            tracing::debug!(
                target: "ezu_graph::eval",
                node = self.graph.node_id(ix),
                op = node.op_name(),
                cache = "hit",
                output = %describe_value(&v),
                tile = %format!("{}/{}/{}", ctx.tile.z, ctx.tile.x, ctx.tile.y),
                "cache hit",
            );
            return Ok((v, key.0));
        }
        // `wasm32-unknown-unknown` has no monotonic clock — `Instant::now()`
        // panics ("time not implemented") — so the per-node timing is
        // host-only. Traces there report `elapsed_us = 0`.
        #[cfg(not(target_arch = "wasm32"))]
        let t0 = Instant::now();
        let value = node.eval(ctx, &input_vals)?;
        #[cfg(not(target_arch = "wasm32"))]
        let elapsed_us = t0.elapsed().as_micros();
        #[cfg(target_arch = "wasm32")]
        let elapsed_us = 0u128;
        tracing::debug!(
            target: "ezu_graph::eval",
            node = self.graph.node_id(ix),
            op = node.op_name(),
            cache = "miss",
            output = %describe_value(&value),
            tile = %format!("{}/{}/{}", ctx.tile.z, ctx.tile.x, ctx.tile.y),
            elapsed_us,
            "evaluated",
        );
        self.cache.insert(key, value.clone());
        Ok((value, key.0))
    }
}

/// Shared, thread-safe scratch for one parallel render: per-node result
/// slots (written once), per-node remaining-input counters, and the first
/// error seen. Each field is `Sync`, so `&ParState` is shared freely
/// across the Rayon scope without further locking of the results.
#[cfg(feature = "parallel")]
struct ParState<'a> {
    slots: Vec<std::sync::OnceLock<(PortValue, Hash128)>>,
    pending: Vec<std::sync::atomic::AtomicUsize>,
    first_err: std::sync::Mutex<Option<RenderError>>,
    ctx: EvalCtx<'a>,
}

/// One-line human-readable summary of a `PortValue` for debug logs.
/// Keeps the format dense so node lines stay readable in a tail.
fn describe_value(v: &PortValue) -> String {
    match v {
        PortValue::Raster(r) => format!("raster {}x{}", r.width, r.height),
        PortValue::Sprite(s) => format!("sprite {}x{}", s.width, s.height),
        PortValue::ScalarField(f) => format!(
            "scalar-field {}x{} (mpp~{:.2})",
            f.width,
            f.height,
            f.metres_per_pixel_x(),
        ),
        PortValue::Features(_) => "features".to_string(),
        PortValue::Brush(_) => "brush".to_string(),
        PortValue::Scalar(s) => format!("scalar {}({:?})", s.kind_name(), s),
    }
}
