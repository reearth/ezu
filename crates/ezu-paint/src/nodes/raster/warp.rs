//! `warp` — domain warp over `Raster|Sprite` (pass-through) using an
//! internal noise field. Same noise dial as the `noise` op (`type`, `scale-px`,
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
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    default_field_seed, raster_or_sprite_output, read_boundary, read_number_or,
    read_optional_string, sample_bilinear, unwrap_raster_or_sprite, wrap_raster_like, Anchor,
    BoundaryMode, ACCEPTS_RASTER_OR_SPRITE,
};
use crate::nodes::raster::noise_field::{fbm, NoiseKind, Sampler};

struct WarpNode {
    kind: NoiseKind,
    scale_px: In<f64>,
    octaves: u32,
    lacunarity: In<f64>,
    gain: In<f64>,
    amp_x: In<f64>,
    amp_y: In<f64>,
    /// Build-time upper bounds on `amp-x-px` / `amp-y-px`, for pad.
    amp_x_bound: f64,
    amp_y_bound: f64,
    seed: Option<u32>,
    anchor: Anchor,
    boundary: BoundaryMode,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for WarpNode {
    fn op_name(&self) -> &'static str {
        "warp"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        raster_or_sprite_output(input_kinds)
    }
    fn coord_space(&self) -> CoordSpace {
        match self.anchor {
            Anchor::World => CoordSpace::World,
            Anchor::Tile => CoordSpace::Inherit,
        }
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        let bump = self.amp_x_bound.abs().max(self.amp_y_bound.abs()).ceil() as u32;
        downstream + bump
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let input = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let (src, kind) = unwrap_raster_or_sprite(input, "input")?;
        let (w, h) = (src.width, src.height);
        let mut out = RasterBuf::new(w, h);

        let scale_px = self.scale_px.get(ctx, inputs)?;
        let lacunarity = self.lacunarity.get(ctx, inputs)?;
        let gain = self.gain.get(ctx, inputs)?;
        let amp_x = self.amp_x.get(ctx, inputs)?;
        let amp_y = self.amp_y.get(ctx, inputs)?;

        // Anchoring decides the default seed: a world-anchored field has to
        // be the same field in every tile, so it cannot use a per-tile one.
        let seed = self
            .seed
            .unwrap_or_else(|| default_field_seed(self.anchor, ctx.rng_seed));
        let nx = Sampler::build(self.kind, seed);
        let ny = Sampler::build(self.kind, seed.wrapping_add(0x9E37_79B9));

        let pad = ctx.canvas.pad as f64;
        let tile_size = ctx.canvas.tile_size as f64;
        let (origin_x, origin_y) = match self.anchor {
            Anchor::World => (ctx.tile.x as f64 * tile_size, ctx.tile.y as f64 * tile_size),
            Anchor::Tile => (0.0, 0.0),
        };
        let inv_scale = if scale_px > 0.0 { 1.0 / scale_px } else { 0.0 };

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
                    lacunarity,
                    gain,
                ) * amp_x;
                let dy = fbm(
                    &ny,
                    px * inv_scale,
                    py * inv_scale,
                    self.octaves,
                    lacunarity,
                    gain,
                ) * amp_y;
                let sx = x as f64 + dx;
                let sy = y as f64 + dy;
                let pxv = sample_bilinear(&src, sx, sy, self.boundary);
                let i = ((y * w + x) * 4) as usize;
                out.pixels[i..i + 4].copy_from_slice(&pxv);
            }
        }
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"warp");
        h.update(&[self.kind.tag()]);
        self.scale_px.param_hash(h);
        h.update(&self.octaves.to_le_bytes());
        self.lacunarity.param_hash(h);
        self.gain.param_hash(h);
        self.amp_x.param_hash(h);
        self.amp_y.param_hash(h);
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
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        // `octaves` stays static: it's clamped to a derived integer range
        // (1..=12) at build time and used as a loop count in eval.
        let octaves = (read_number_or(fields, "octaves", ctx, 1.0)? as u32).clamp(1, 12);

        let mut r = InReader::new(fields, ctx, 1);
        let scale_px = r.number("scale-px")?;
        let lacunarity = r.number_or("lacunarity", 2.0)?;
        let gain = r.number_or("gain", 0.5)?;
        // `amp-px` is the shared default for the per-axis amplitudes; when
        // `amp-x-px` / `amp-y-px` are omitted they inherit `amp-px` as-is
        // (same literal / `$param` / `@node` binding).
        let amp = r.number("amp-px")?;
        let amp_x = if fields.contains_key("amp-x-px") {
            r.number("amp-x-px")?
        } else {
            amp.clone()
        };
        let amp_y = if fields.contains_key("amp-y-px") {
            r.number("amp-y-px")?
        } else {
            amp.clone()
        };
        let parts = r.finish();

        // scale-px must be > 0; check the static bound (literal value, or a
        // `$param`'s declared `max`). A `@node` port has no static bound, so
        // its value is guarded at eval (treated as no-warp when <= 0).
        if let Some(b) = scale_px.static_bound() {
            if b <= 0.0 {
                return Err(FactoryError::BadField {
                    field: "scale-px".into(),
                    msg: "scale-px must be > 0".into(),
                });
            }
        }
        let amp_x_bound = amp_x.static_bound().ok_or_else(|| FactoryError::BadField {
            field: "amp-x-px".into(),
            msg: "pad depends on amp-x-px (or amp-px) at build time: use a literal, or a \
                  `$param` with `max` (a `@node` port has no static bound)"
                .into(),
        })?;
        let amp_y_bound = amp_y.static_bound().ok_or_else(|| FactoryError::BadField {
            field: "amp-y-px".into(),
            msg: "pad depends on amp-y-px (or amp-px) at build time: use a literal, or a \
                  `$param` with `max` (a `@node` port has no static bound)"
                .into(),
        })?;

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

        let mut ports = vec![PortSpec {
            name: "input",
            accepts: ACCEPTS_RASTER_OR_SPRITE,
            optional: false,
        }];
        ports.extend(parts.ports);
        let mut connections = vec![Connection {
            port: "input".into(),
            src: input,
        }];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(WarpNode {
                kind,
                scale_px,
                octaves,
                lacunarity,
                gain,
                amp_x,
                amp_y,
                amp_x_bound,
                amp_y_bound,
                seed,
                anchor,
                boundary,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
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
                "lacunarity": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 2.0 })),
                "gain": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 0.5 })),
                "amp-px": schema_frag::px_number(),
                "amp-x-px": schema_frag::px_number(),
                "amp-y-px": schema_frag::px_number(),
                "seed": { "type": "integer", "minimum": 0, "description": "Field seed. Omitted, `anchor: world` uses a fixed seed so every tile samples the same field, and `anchor: tile` uses the host's per-tile seed so each tile gets its own. Set it to pin either." },
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
