//! `mosaic` — `Raster -> Raster`. Quantize the input into uniform
//! square blocks: each block is filled with the average color of the
//! source pixels it covers.
//!
//! With `anchor: "world"` (default) the block grid is anchored to z=0
//! world pixels, so adjacent map tiles see the same block boundaries
//! and seams disappear. This requires the upstream to supply at least
//! `block` extra padding so blocks straddling the tile edge can be
//! averaged from the same source pixels in both tiles. With
//! `anchor: "tile"` each map tile starts the grid at its own top-left
//! and no extra padding is needed (but tile seams may be visible).

use std::collections::HashMap;
use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{read_number, read_optional_string, Anchor};

struct MosaicNode {
    block: u32,
    anchor: Anchor,
}

impl Node for MosaicNode {
    fn op_name(&self) -> &'static str {
        "mosaic"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "input",
            accepts: &[PortKind::Raster],
            optional: false,
        }];
        SPECS
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
    }
    fn coord_space(&self) -> CoordSpace {
        match self.anchor {
            Anchor::World => CoordSpace::World,
            Anchor::Tile => CoordSpace::Tile,
        }
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        // World-anchored blocks may straddle the tile edge; growing the
        // upstream pad by `block` keeps the straddling block fully
        // visible in every tile that touches it, so the average is the
        // same on both sides of the seam.
        match self.anchor {
            Anchor::World => downstream.saturating_add(self.block),
            Anchor::Tile => downstream,
        }
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let src = inputs[0]
            .as_ref()
            .and_then(PortValue::as_raster)
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let block = self.block.max(1) as i64;
        if block == 1 {
            return Ok(PortValue::Raster(src.clone()));
        }
        let w = src.width as i64;
        let h = src.height as i64;
        let pad = ctx.canvas.pad as i64;
        let tile_size = ctx.canvas.tile_size as i64;

        // World coordinate of source pixel (0, 0).
        let (origin_x, origin_y) = match self.anchor {
            Anchor::World => (
                ctx.tile.x as i64 * tile_size - pad,
                ctx.tile.y as i64 * tile_size - pad,
            ),
            Anchor::Tile => (0, 0),
        };

        // Cache per-block averages keyed by world block index. Each
        // block is averaged once and reused for every output pixel
        // inside it.
        let mut cache: HashMap<(i64, i64), [u8; 4]> = HashMap::new();
        let src_px = &src.pixels;
        let mut out = RasterBuf::new(src.width, src.height);
        let dst = &mut out.pixels;

        for y in 0..h {
            let world_y = origin_y + y;
            let by = world_y.div_euclid(block);
            for x in 0..w {
                let world_x = origin_x + x;
                let bx = world_x.div_euclid(block);
                let color = *cache.entry((bx, by)).or_insert_with(|| {
                    // World-space block extent.
                    let wx0 = bx * block;
                    let wy0 = by * block;
                    let wx1 = wx0 + block;
                    let wy1 = wy0 + block;
                    // Clip to the available source pixels (in source-local
                    // coords).
                    let sx0 = (wx0 - origin_x).max(0);
                    let sy0 = (wy0 - origin_y).max(0);
                    let sx1 = (wx1 - origin_x).min(w);
                    let sy1 = (wy1 - origin_y).min(h);
                    if sx1 <= sx0 || sy1 <= sy0 {
                        return [0; 4];
                    }
                    let mut sum = [0u64; 4];
                    let mut count = 0u64;
                    for yy in sy0..sy1 {
                        let row = (yy * w) as usize * 4;
                        for xx in sx0..sx1 {
                            let i = row + (xx as usize) * 4;
                            sum[0] += src_px[i] as u64;
                            sum[1] += src_px[i + 1] as u64;
                            sum[2] += src_px[i + 2] as u64;
                            sum[3] += src_px[i + 3] as u64;
                            count += 1;
                        }
                    }
                    [
                        (sum[0] / count) as u8,
                        (sum[1] / count) as u8,
                        (sum[2] / count) as u8,
                        (sum[3] / count) as u8,
                    ]
                });
                let i = ((y * w + x) as usize) * 4;
                dst[i] = color[0];
                dst[i + 1] = color[1];
                dst[i + 2] = color[2];
                dst[i + 3] = color[3];
            }
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"mosaic");
        h.update(&self.block.to_le_bytes());
        match self.anchor {
            Anchor::World => h.update(b"w"),
            Anchor::Tile => h.update(b"t"),
        }
    }
}

pub(super) struct MosaicFactory;
impl NodeFactory for MosaicFactory {
    fn op_name(&self) -> &'static str {
        "mosaic"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let block_raw = read_number(fields, "block", ctx)?;
        if !(block_raw.is_finite() && block_raw >= 1.0) {
            return Err(FactoryError::BadField {
                field: "block".into(),
                msg: "expected integer >= 1".into(),
            });
        }
        let block = block_raw.round() as u32;
        let anchor = match read_optional_string(fields, "anchor")?.as_deref() {
            None | Some("world") => Anchor::World,
            Some("tile") => Anchor::Tile,
            Some(other) => {
                return Err(FactoryError::BadField {
                    field: "anchor".into(),
                    msg: format!("expected `world` or `tile`, got `{other}`"),
                });
            }
        };
        Ok(BuiltNode {
            node: Box::new(MosaicNode { block, anchor }),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Pixelate a raster into uniform square blocks; each block is filled with the average color of the source pixels it covers. World-anchored by default so adjacent map tiles share the same block grid.",
            "properties": {
                "input": schema_frag::node_ref(),
                "block": { "type": "integer", "minimum": 1,
                           "description": "Block edge length in canvas pixels." },
                "anchor": { "type": "string", "enum": ["world", "tile"], "default": "world",
                            "description": "`world` (default) makes the block grid seamless across map tiles by growing the upstream pad. `tile` restarts the grid at every map tile's top-left and requires no extra padding." },
            },
            "required": ["input", "block"],
        })
    }
}

ezu_graph::submit_node!(MosaicFactory);
