//! Walk the DAG and evaluate one tile.

use xxhash_rust::xxh3::Xxh3;

use crate::cache::{Cache, CacheKey, Hash128};
use crate::eval::{AssetLoader, CanvasInfo, EvalCtx, EvalError, ParamValues, TileId};
use crate::graph::Graph;
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
    pub fn render(
        &self,
        tile: TileId,
        canvas: CanvasInfo,
        params: &ParamValues,
        rng_seed: u64,
    ) -> Result<PortValue, RenderError> {
        self.render_with_tile_data(tile, canvas, params, rng_seed, None)
    }

    /// Like [`render`] but supplies host-side tile data (e.g. a decoded
    /// MVT) to source nodes via [`EvalCtx::tile_data`].
    pub fn render_with_tile_data(
        &self,
        tile: TileId,
        canvas: CanvasInfo,
        params: &ParamValues,
        rng_seed: u64,
        tile_data: Option<&crate::buf::OpaqueValue>,
    ) -> Result<PortValue, RenderError> {
        let n = self.graph.len();
        let mut hashes: Vec<Hash128> = vec![0; n];
        let mut values: Vec<Option<PortValue>> = vec![None; n];

        let ctx = EvalCtx {
            tile,
            canvas,
            assets: self.assets,
            params,
            rng_seed,
            tile_data,
        };

        for &ix in self.graph.topo_order() {
            let node = self.graph.node(ix);

            // Hash this node's own params.
            let mut h = Xxh3::new();
            node.param_hash(&mut h);
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
                _ => Some(tile),
            };
            let key = CacheKey::build(canvas, tile_for_key, params_hash, &input_hashes);
            hashes[ix] = key.0;

            // Cache hit? skip eval.
            if let Some(v) = self.cache.get(key) {
                values[ix] = Some(v);
                continue;
            }

            let value = node.eval(&ctx, &input_vals)?;
            self.cache.insert(key, value.clone());
            values[ix] = Some(value);
        }

        Ok(values[self.graph.output()].clone().expect("output unset"))
    }
}
