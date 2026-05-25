//! `warp` — `Raster -> Raster`. Domain warp using an internal noise
//! field. Same noise dial as the `noise` op (`type`, `scale-px`,
//! `octaves`, `lacunarity`, `gain`, `seed`, `anchor`), plus `amp-px`
//! for displacement magnitude and a boundary mode.
//!
//! With `anchor: world` (default) the noise field is sampled in global
//! pixel coordinates so adjacent tiles agree on the displacement at
//! the shared border. The upstream pad grows by `amp-px` to keep
//! samples inside the available raster.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    read_boundary, read_number, read_number_or, read_optional_string, sample_bilinear, Anchor,
    BoundaryMode,
};
use crate::nodes::raster::noise_field::{fbm, NoiseKind, Sampler};

struct WarpNode {
    kind: NoiseKind,
    scale_px: f64,
    octaves: u32,
    lacunarity: f64,
    gain: f64,
    amp_x: f64,
    amp_y: f64,
    seed: Option<u32>,
    anchor: Anchor,
    boundary: BoundaryMode,
}

impl Node for WarpNode {
    fn op_name(&self) -> &'static str {
        "warp"
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
            Anchor::Tile => CoordSpace::Inherit,
        }
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        let bump = self.amp_x.abs().max(self.amp_y.abs()).ceil() as u32;
        downstream + bump
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
        let (w, h) = (src.width, src.height);
        let mut out = RasterBuf::new(w, h);

        let seed = self
            .seed
            .unwrap_or((ctx.rng_seed as u32) ^ ((ctx.rng_seed >> 32) as u32));
        let nx = Sampler::build(self.kind, seed);
        let ny = Sampler::build(self.kind, seed.wrapping_add(0x9E37_79B9));

        let pad = ctx.canvas.pad as f64;
        let tile_size = ctx.canvas.tile_size as f64;
        let (origin_x, origin_y) = match self.anchor {
            Anchor::World => (ctx.tile.x as f64 * tile_size, ctx.tile.y as f64 * tile_size),
            Anchor::Tile => (0.0, 0.0),
        };
        let inv_scale = 1.0 / self.scale_px;

        for y in 0..h {
            // `py` is the world-pixel coord at the current zoom (or tile-
            // local if anchor=tile); subtract pad so the visible tile area
            // sits at (0..tile_size, 0..tile_size) in the noise input.
            let py = origin_y + (y as f64) - pad;
            for x in 0..w {
                let px = origin_x + (x as f64) - pad;
                let dx = fbm(
                    &nx,
                    px * inv_scale,
                    py * inv_scale,
                    self.octaves,
                    self.lacunarity,
                    self.gain,
                ) * self.amp_x;
                let dy = fbm(
                    &ny,
                    px * inv_scale,
                    py * inv_scale,
                    self.octaves,
                    self.lacunarity,
                    self.gain,
                ) * self.amp_y;
                let sx = x as f64 + dx;
                let sy = y as f64 + dy;
                let pxv = sample_bilinear(src, sx, sy, self.boundary);
                let i = ((y * w + x) * 4) as usize;
                out.pixels[i..i + 4].copy_from_slice(&pxv);
            }
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"warp");
        h.update(&[self.kind.tag()]);
        h.update(&self.scale_px.to_le_bytes());
        h.update(&self.octaves.to_le_bytes());
        h.update(&self.lacunarity.to_le_bytes());
        h.update(&self.gain.to_le_bytes());
        h.update(&self.amp_x.to_le_bytes());
        h.update(&self.amp_y.to_le_bytes());
        match self.seed {
            Some(s) => {
                h.update(&[1]);
                h.update(&s.to_le_bytes());
            }
            None => h.update(&[0]),
        }
        h.update(match self.anchor {
            Anchor::Tile => &[0u8],
            Anchor::World => &[1u8],
        });
        h.update(&[match self.boundary {
            BoundaryMode::Clamp => 0,
            BoundaryMode::Transparent => 1,
            BoundaryMode::Mirror => 2,
        }]);
    }
}

pub(super) struct WarpFactory;
impl NodeFactory for WarpFactory {
    fn op_name(&self) -> &'static str {
        "warp"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let kind = match read_optional_string(fields, "type")?.as_deref() {
            None => NoiseKind::Perlin,
            Some(s) => NoiseKind::parse(s).ok_or_else(|| FactoryError::BadField {
                field: "type".into(),
                msg: format!(
                    "unknown noise type `{s}`, expected white/value/perlin/simplex/worley"
                ),
            })?,
        };
        let scale_px = read_number(fields, "scale-px", ctx)?;
        if scale_px <= 0.0 {
            return Err(FactoryError::BadField {
                field: "scale-px".into(),
                msg: "scale-px must be > 0".into(),
            });
        }
        let octaves = (read_number_or(fields, "octaves", ctx, 1.0)? as u32).clamp(1, 12);
        let lacunarity = read_number_or(fields, "lacunarity", ctx, 2.0)?;
        let gain = read_number_or(fields, "gain", ctx, 0.5)?;
        let amp = read_number(fields, "amp-px", ctx)?;
        let amp_x = read_number_or(fields, "amp-x-px", ctx, amp)?;
        let amp_y = read_number_or(fields, "amp-y-px", ctx, amp)?;
        let seed = match fields.get("seed") {
            None => None,
            Some(v) if v.is_null() => None,
            Some(v) => Some(v.as_u64().ok_or_else(|| FactoryError::BadField {
                field: "seed".into(),
                msg: "expected non-negative integer".into(),
            })? as u32),
        };
        let anchor = match read_optional_string(fields, "anchor")?.as_deref() {
            None | Some("world") => Anchor::World,
            Some("tile") => Anchor::Tile,
            Some(other) => {
                return Err(FactoryError::BadField {
                    field: "anchor".into(),
                    msg: format!("unknown anchor `{other}`, expected tile/world"),
                });
            }
        };
        let boundary = read_boundary(fields, "boundary", BoundaryMode::Clamp)?;
        Ok(BuiltNode {
            node: Box::new(WarpNode {
                kind,
                scale_px,
                octaves,
                lacunarity,
                gain,
                amp_x,
                amp_y,
                seed,
                anchor,
                boundary,
            }),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Domain warp via an internal noise field. With `anchor: world` (default) the warp is seamless across tile borders; the upstream pad grows by `amp-px` to keep samples inside the available raster.",
            "properties": {
                "input": schema_frag::node_ref(),
                "type": {
                    "type": "string",
                    "enum": ["white", "value", "perlin", "simplex", "worley"],
                    "default": "perlin",
                },
                "scale-px": schema_frag::px_number(),
                "octaves": { "type": "integer", "minimum": 1, "maximum": 12, "default": 1 },
                "lacunarity": { "type": "number", "default": 2.0 },
                "gain": { "type": "number", "default": 0.5 },
                "amp-px": schema_frag::px_number(),
                "amp-x-px": schema_frag::px_number(),
                "amp-y-px": schema_frag::px_number(),
                "seed": { "type": "integer", "minimum": 0 },
                "anchor": { "type": "string", "enum": ["tile", "world"], "default": "world" },
                "boundary": {
                    "type": "string",
                    "enum": ["clamp", "transparent", "mirror"],
                    "default": "clamp",
                },
            },
            "required": ["input", "scale-px", "amp-px"],
        })
    }
}

ezu_graph::submit_node!(WarpFactory);
