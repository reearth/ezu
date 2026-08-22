//! `point-scatter` — `() -> Features`. A stochastic point set covering the
//! current tile, at a chosen mean spacing.
//!
//! The sibling op `point-grid` emits an exact lattice, and jittering those
//! points does not make the result look random: a jittered lattice still has
//! exactly one point per cell, so the count per cell has no variance at all.
//! The density is perfectly even, and the pitch survives — the eye finds it,
//! and so does a Fourier transform. What this op adds is the missing
//! variance: each cell draws a *count* of 0 to 3 points, so the pattern gets
//! the clumps and gaps of a genuinely random set.
//!
//! The two anchor modes match `point-grid`:
//!
//! - `world` — cells are indexed off global `(0, 0)`, so neighbouring tiles
//!   draw the same points in the region they share and the pattern is
//!   seamless.
//! - `tile` — cells are indexed off the tile's own `(0, 0)`, so every tile
//!   gets the same point set. Cheap and repeatable, but the seams show.
//!
//! Spacing is in tile-local pixel units (the same `extent` used
//! downstream). The point set covers the padded canvas, so points just
//! outside the tile exist and whatever draws them spills in.

use ezu_core::seed::{cell_seed, next_unit};
use ezu_graph::{
    schema_frag, BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, In, InReader,
    Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{features_value, read_number, read_optional_string, FeatureGroup};

const DEFAULT_EXTENT: u32 = 4096;

/// Salt for the per-cell seed when the style names none, so the scatter is
/// stable across tiles and across runs without anyone asking for it.
const DEFAULT_SEED: u32 = 0x5343_4154; // 'SCAT'

/// Cumulative thresholds for the per-cell count: 0 points with probability
/// 6/16, 1 with 6/16, 2 with 2/16, 3 with 2/16.
///
/// Poisson(1) is the distribution a truly random point process gives, and
/// these four weights reproduce its first two moments exactly — mean 1 and
/// variance 1 — which is what makes the density right on average and uneven
/// in the way that matters. Simply truncating Poisson(1) at 3 and
/// renormalising would instead give a mean of 0.94, quietly thinning the
/// pattern by 6%. Stopping at 3 keeps the per-cell work bounded; the tail it
/// gives up is worth 2% of Poisson's mass.
const COUNT_CDF: [f32; 3] = [6.0 / 16.0, 12.0 / 16.0, 14.0 / 16.0];

#[inline]
fn count_for(u: f32) -> u32 {
    match u {
        u if u < COUNT_CDF[0] => 0,
        u if u < COUNT_CDF[1] => 1,
        u if u < COUNT_CDF[2] => 2,
        _ => 3,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    Tile,
    World,
}

struct PointScatterNode {
    extent: u32,
    spacing_x: In<f64>,
    spacing_y: In<f64>,
    anchor: Anchor,
    seed: u32,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for PointScatterNode {
    fn op_name(&self) -> &'static str {
        "point-scatter"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Features
    }
    fn coord_space(&self) -> CoordSpace {
        CoordSpace::Tile
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let e = self.extent as f64;
        let spacing_x = self.spacing_x.get(ctx, inputs)?;
        let spacing_y = self.spacing_y.get(ctx, inputs)?;
        // A non-positive spacing has no cells; emit nothing rather than
        // diverge.
        if spacing_x <= 0.0 || spacing_y <= 0.0 {
            return Ok(features_value(self.extent, vec![]));
        }
        // Where this tile sits in the world, in extent units. The cell index
        // is derived from the world coordinate, so under `world` anchoring
        // two tiles covering one cell agree on its contents. The offset is a
        // whole number of extents, which is also why the rounding below
        // cannot disagree between neighbours.
        let (tox, toy) = match self.anchor {
            Anchor::Tile => (0.0, 0.0),
            Anchor::World => ((ctx.tile.x as f64) * e, (ctx.tile.y as f64) * e),
        };
        // Cover the padded canvas, not the visible tile: ops with an extent
        // around each point (a sprite in `stamp`, a radius in `circles`) need
        // the points just outside the tile to draw their spill into it, or
        // every tile border shows a seam. Same computation as `point-grid`,
        // so the two ops agree on what "just outside" means.
        let margin_x = (ctx.canvas.pad as f64 * e / ctx.canvas.tile_w.max(1) as f64).ceil();
        let margin_y = (ctx.canvas.pad as f64 * e / ctx.canvas.tile_h.max(1) as f64).ceil();
        // Every cell that meets the padded area. A cell's points land inside
        // its own bounds, so these are all the cells that can contribute.
        let i0 = ((tox - margin_x) / spacing_x).floor() as i64;
        let i1 = ((tox + e + margin_x) / spacing_x).floor() as i64;
        let j0 = ((toy - margin_y) / spacing_y).floor() as i64;
        let j1 = ((toy + e + margin_y) / spacing_y).floor() as i64;

        let mut points = Vec::new();
        let mut j = j0;
        while j <= j1 {
            let mut i = i0;
            while i <= i1 {
                // Seeded from the integer cell index alone: no dependence on
                // iteration order, on the tile being drawn, or on any
                // floating-point world position that two tiles might round
                // differently.
                let mut state = cell_seed(i, j, self.seed);
                let n = count_for(next_unit(&mut state));
                for _ in 0..n {
                    let u = next_unit(&mut state) as f64;
                    let v = next_unit(&mut state) as f64;
                    // Emitted wherever in its cell the point landed, even a
                    // little past the padded edge: clipping here would drop a
                    // point whose sprite still reaches the canvas, and — worse
                    // — the two tiles sharing that point would disagree about
                    // whether it exists.
                    let x = (i as f64 + u) * spacing_x - tox;
                    let y = (j as f64 + v) * spacing_y - toy;
                    points.push((x.round() as i32, y.round() as i32));
                }
                i += 1;
            }
            j += 1;
        }
        Ok(features_value(
            self.extent,
            vec![FeatureGroup::synthetic(vec![], vec![], points)],
        ))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"point-scatter");
        h.update(&self.extent.to_le_bytes());
        self.spacing_x.param_hash(h);
        self.spacing_y.param_hash(h);
        h.update(match self.anchor {
            Anchor::Tile => &[0u8],
            Anchor::World => &[1u8],
        });
        h.update(&self.seed.to_le_bytes());
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct PointScatterFactory;
impl NodeFactory for PointScatterFactory {
    fn op_name(&self) -> &'static str {
        "point-scatter"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let extent = fields
            .get("extent")
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .unwrap_or(DEFAULT_EXTENT);
        // `spacing` is the build-time default for the per-axis spacings;
        // it is never stored on the node, so it stays a static literal.
        let spacing = read_number(fields, "spacing", ctx)?;
        let anchor = match read_optional_string(fields, "anchor")?.as_deref() {
            None | Some("world") => Anchor::World,
            Some("tile") => Anchor::Tile,
            Some(other) => {
                return Err(FactoryError::BadField {
                    field: "anchor".into(),
                    msg: format!("unknown anchor '{other}', expected tile/world"),
                });
            }
        };
        let seed = match fields.get("seed") {
            Some(Value::Number(n)) => n.as_u64().map(|v| v as u32).unwrap_or(DEFAULT_SEED),
            _ => DEFAULT_SEED,
        };

        let mut r = InReader::new(fields, ctx, 0);
        let spacing_x = r.number_or("spacing-x", spacing)?;
        let spacing_y = r.number_or("spacing-y", spacing)?;
        let parts = r.finish();

        // Spacing must be > 0; check the static bounds (literal, or a
        // `$param`'s declared `max`). A `@node` port has no static bound —
        // eval emits an empty set for non-positive values instead.
        for (name, sp) in [("spacing-x", &spacing_x), ("spacing-y", &spacing_y)] {
            if let Some(b) = sp.static_bound() {
                if b <= 0.0 {
                    return Err(FactoryError::BadField {
                        field: name.into(),
                        msg: "spacing must be > 0".into(),
                    });
                }
            }
        }

        Ok(BuiltNode {
            node: Box::new(PointScatterNode {
                extent,
                spacing_x,
                spacing_y,
                anchor,
                seed,
                ports: parts.ports,
                param_refs: parts.param_refs,
            }),
            connections: parts.connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Random points at a given mean spacing, covering the tile. Where `point-grid` puts exactly one point in the middle of every cell, this one draws a *count* per cell — 0 to 3, averaging 1 — and places each point at a random spot inside it, so the set has the clumps and gaps of a real random scatter and no residual pitch. Reach for `point-grid` when the regularity is the point (a halftone screen, a tiling of panes to fracture) and for `point-scatter` when it is the enemy (stipple, foliage, grain, scattered symbols). Jittering a `point-grid` is not a substitute: one point per cell means an even density, and the lattice frequency stays plainly visible.",
            "properties": {
                "extent": { "type": "integer", "minimum": 1, "default": DEFAULT_EXTENT },
                "spacing": schema_frag::in_number(serde_json::json!({
                    "type": "number", "minimum": 0.0,
                    "description": "Mean spacing in *extent* units, not pixels — one point per `spacing` × `spacing` cell on average. `extent` below is the coordinate space (4096 by default), so a 512 px tile averages 8 points across at `spacing: 512`. Paint ops downstream take pixels, so the two do not match."
                })),
                "spacing-x": schema_frag::px_number(),
                "spacing-y": schema_frag::px_number(),
                "anchor": { "type": "string", "enum": ["tile", "world"], "default": "world",
                            "description": "`world` indexes the cells off global (0, 0), so adjacent tiles agree on the points they share and the scatter is seamless — the usual choice. `tile` indexes off the tile's own corner, giving every tile the same point set and a visible seam." },
                "seed": { "type": "integer", "minimum": 0,
                          "description": "Optional explicit u32 seed. Change it to get a different scatter at the same spacing, or to keep two `point-scatter` nodes from landing on each other. Default: a fixed constant, so the pattern is stable across tiles and runs." },
            },
            "required": ["spacing"],
        })
    }
}

ezu_graph::submit_node!(PointScatterFactory);
