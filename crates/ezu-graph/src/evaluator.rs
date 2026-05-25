//! Walk the DAG and evaluate one tile.

use std::time::Instant;

use xxhash_rust::xxh3::Xxh3;

use crate::cache::{Cache, CacheKey, Hash128};
use crate::eval::{AssetLoader, CanvasInfo, EvalCtx, EvalError, ParamValues, TileId};
use crate::graph::{Graph, NodeIx};
use crate::port::{CoordSpace, PortKind};
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
            let (value, hash) = self.eval_one(ix, &ctx, &hashes, &values)?;
            hashes[ix] = hash;
            values[ix] = Some(value);
        }
        Ok(values[self.graph.output()].clone().expect("output unset"))
    }

    /// Like [`render`] but evaluates nodes in parallel per topological
    /// "level" using Rayon. All nodes at the same level have no edges
    /// between them, so they fan out across the global thread pool with
    /// no synchronization cost beyond a per-level join.
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
            use rayon::prelude::*;

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

            for bucket in self.graph.level_buckets() {
                let new: Vec<(NodeIx, PortValue, Hash128)> = bucket
                    .par_iter()
                    .map(|&ix| -> Result<_, RenderError> {
                        let (v, h) = self.eval_one(ix, &ctx, &hashes, &values)?;
                        Ok((ix, v, h))
                    })
                    .collect::<Result<_, _>>()?;
                for (ix, v, h) in new {
                    values[ix] = Some(v);
                    hashes[ix] = h;
                }
            }
            Ok(values[self.graph.output()].clone().expect("output unset"))
        }
    }

    /// Evaluate one node given the current intermediate state. Pulled
    /// out so the serial and parallel paths share the cache lookup and
    /// hashing logic.
    fn eval_one(
        &self,
        ix: NodeIx,
        ctx: &EvalCtx<'_>,
        hashes: &[Hash128],
        values: &[Option<PortValue>],
    ) -> Result<(PortValue, Hash128), RenderError> {
        let node = self.graph.node(ix);

        // Hash this node's own params, plus any asset bindings it samples.
        let mut h = Xxh3::new();
        node.param_hash(&mut h);
        for name in node.asset_inputs() {
            h.update(name.as_bytes());
            h.update(&ctx.assets.hash(&name).to_le_bytes());
        }
        let params_hash: Hash128 = h.digest128();

        // Collect input hashes (in port order) and input values.
        let input_specs = node.inputs();
        let mut input_hashes: Vec<Hash128> = Vec::with_capacity(input_specs.len());
        let mut input_vals: Vec<Option<PortValue>> = Vec::with_capacity(input_specs.len());
        for port_ix in 0..input_specs.len() {
            match self.graph.incoming(ix, port_ix) {
                Some(src) => {
                    input_hashes.push(hashes[src]);
                    input_vals.push(values[src].clone());
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
        let t0 = Instant::now();
        let value = node.eval(ctx, &input_vals)?;
        let elapsed_us = t0.elapsed().as_micros();
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
        PortValue::Scalar(_) => format!("scalar:{}", PortKind::Scalar),
    }
}
