//! `contour` — `ScalarField -> Features`. Isolines extracted from a
//! scalar field by marching squares with linear sub-cell interpolation,
//! chained into polylines. Enables `dem → contour → stroke` (elevation
//! contours) and `features → density → contour → stroke` (point-density
//! isolines).
//!
//! Levels come from `interval` (+ optional `base`) or an explicit
//! `levels` array, optionally clamped by `min` / `max`. Each level
//! becomes one feature group whose polylines are converted back to
//! tile-local coordinates (inverting the canvas-px scaling, so
//! downstream paint re-scales them to the same pixels) and whose
//! properties carry `{"level": <number>}` — so data-driven paint can
//! style by level (index contours via `width-expr`, `filter-expr`, …).
//!
//! Fields are computed on the padded canvas, so isolines extend through
//! the pad region; neighbouring tiles compute the same values in the
//! overlap and their contours agree at the seam.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue, ScalarField,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{features_value, read_number, FeatureGroup};

/// Extent of the emitted tile-local coordinates. 4096 is the MVT
/// convention; at a 512px tile that is 8 units per pixel, so the
/// canvas-px → tile round-trip stays well under half a pixel.
const OUTPUT_EXTENT: u32 = 4096;

/// Guard against an `interval` far smaller than the field's range
/// producing an absurd number of levels.
const MAX_LEVELS: usize = 512;

struct ContourNode {
    interval: In<f64>,
    base: In<f64>,
    /// Explicit level list; overrides `interval` / `base` when present.
    levels: Option<Vec<In<f64>>>,
    /// Clamp on which levels are emitted.
    min: Option<f64>,
    max: Option<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for ContourNode {
    fn op_name(&self) -> &'static str {
        "contour"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Features
    }
    fn coord_space(&self) -> CoordSpace {
        // Output features are tile-local, like every `Features` producer.
        CoordSpace::Tile
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let field = inputs[0]
            .as_ref()
            .and_then(PortValue::as_scalar_field)
            .ok_or_else(|| EvalError::MissingInput("field".into()))?;
        if field.width < 2 || field.height < 2 {
            return Ok(features_value(OUTPUT_EXTENT, vec![]));
        }
        let levels = self.resolve_levels(ctx, inputs, field)?;

        // Canvas px → tile-local units: field sample (x, y) sits at pixel
        // center (x + 0.5, y + 0.5); invert the paint-side scaling
        // `px = t · tile/extent + pad`.
        let pad = ctx.canvas.pad as f32;
        let tile = ctx.canvas.tile_w.max(1) as f32;
        let to_tile = |(gx, gy): (f32, f32)| -> (i32, i32) {
            let ex = (gx + 0.5 - pad) / tile * OUTPUT_EXTENT as f32;
            let ey = (gy + 0.5 - pad) / tile * OUTPUT_EXTENT as f32;
            (ex.round() as i32, ey.round() as i32)
        };

        let mut groups = Vec::new();
        for level in levels {
            let segments = marching_squares(field, level as f32);
            let mut lines: Vec<Vec<(i32, i32)>> = Vec::new();
            for polyline in chain_segments(segments) {
                let mut line: Vec<(i32, i32)> = Vec::with_capacity(polyline.len());
                for p in polyline {
                    let q = to_tile(p);
                    // Extent-unit rounding can collapse neighbouring
                    // vertices; keep the polyline simple.
                    if line.last() != Some(&q) {
                        line.push(q);
                    }
                }
                if line.len() >= 2 {
                    lines.push(line);
                }
            }
            if lines.is_empty() {
                continue;
            }
            let properties: BTreeMap<String, maplibre_expr::Value> =
                [("level".to_string(), maplibre_expr::Value::Number(level))].into();
            groups.push(FeatureGroup {
                properties: Arc::new(properties),
                polygons: vec![],
                lines,
                points: vec![],
            });
        }
        Ok(features_value(OUTPUT_EXTENT, groups))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"contour");
        self.interval.param_hash(h);
        self.base.param_hash(h);
        if let Some(levels) = &self.levels {
            h.update(b"levels");
            for l in levels {
                l.param_hash(h);
            }
        }
        for (tag, bound) in [(b"min".as_slice(), self.min), (b"max".as_slice(), self.max)] {
            if let Some(v) = bound {
                h.update(tag);
                h.update(&v.to_le_bytes());
            }
        }
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

impl ContourNode {
    /// The ascending list of levels to extract: the explicit `levels`
    /// array, or `base + k·interval` for every `k` whose level falls
    /// inside the field's value range — both clamped by `min` / `max`.
    fn resolve_levels(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
        field: &ScalarField,
    ) -> Result<Vec<f64>, EvalError> {
        let keep = |l: f64| self.min.is_none_or(|m| l >= m) && self.max.is_none_or(|m| l <= m);
        if let Some(levels) = &self.levels {
            // A level may be a `$param`, so resolve before filtering and
            // sorting — the order is only knowable now.
            let mut out = Vec::with_capacity(levels.len());
            for l in levels {
                let l = l.get(ctx, inputs)?;
                if keep(l) {
                    out.push(l);
                }
            }
            out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            out.dedup();
            return Ok(out);
        }
        let interval = self.interval.get(ctx, inputs)?;
        if interval <= 0.0 {
            return Ok(vec![]);
        }
        let base = self.base.get(ctx, inputs)?;
        // Field value range (nodata / non-finite samples don't count).
        let nodata = field.nodata;
        let mut range: Option<(f64, f64)> = None;
        for &v in field.values.iter() {
            if !v.is_finite() || nodata == Some(v) {
                continue;
            }
            let v = v as f64;
            range = Some(range.map_or((v, v), |(lo, hi)| (lo.min(v), hi.max(v))));
        }
        let Some((lo, hi)) = range else {
            return Ok(vec![]);
        };
        let k0 = ((lo - base) / interval).ceil() as i64;
        let k1 = ((hi - base) / interval).floor() as i64;
        if k1 < k0 {
            return Ok(vec![]);
        }
        if (k1 - k0) as usize >= MAX_LEVELS {
            return Err(EvalError::Other(format!(
                "contour: interval {interval} yields more than {MAX_LEVELS} levels over the \
                 field's range [{lo}, {hi}]"
            )));
        }
        Ok((k0..=k1)
            .map(|k| base + k as f64 * interval)
            .filter(|&l| keep(l))
            .collect())
    }
}

/// One directed isoline segment in grid coordinates (field sample
/// indices; sub-cell positions are fractional).
type Segment = ((f32, f32), (f32, f32));

/// Marching squares over the field at `level`. Segments are oriented
/// consistently (values above the level on the left), so they chain
/// head-to-tail. Cells touching non-finite / nodata samples emit
/// nothing.
fn marching_squares(field: &ScalarField, level: f32) -> Vec<Segment> {
    let (w, h) = (field.width, field.height);
    let nodata = field.nodata;
    let at = |x: u32, y: u32| -> f32 {
        let v = field.values[(y * w + x) as usize];
        if nodata == Some(v) {
            f32::NAN
        } else {
            v
        }
    };
    // Interpolated crossing on the edge from value `va` (at offset 0)
    // to `vb` (at offset 1). Both cells sharing an edge compute this
    // with identical operands, so shared endpoints match bit-for-bit.
    let lerp = |va: f32, vb: f32| -> f32 { (level - va) / (vb - va) };

    let mut segments = Vec::new();
    for y in 0..h - 1 {
        for x in 0..w - 1 {
            // Corners: TL (x, y), TR, BR, BL.
            let v0 = at(x, y);
            let v1 = at(x + 1, y);
            let v2 = at(x + 1, y + 1);
            let v3 = at(x, y + 1);
            if !(v0.is_finite() && v1.is_finite() && v2.is_finite() && v3.is_finite()) {
                continue;
            }
            let case = ((v0 > level) as u8) << 3
                | ((v1 > level) as u8) << 2
                | ((v2 > level) as u8) << 1
                | ((v3 > level) as u8);
            if case == 0 || case == 15 {
                continue;
            }
            let (xf, yf) = (x as f32, y as f32);
            // Edge crossings (only valid for the cases that use them).
            let t = || (xf + lerp(v0, v1), yf);
            let r = || (xf + 1.0, yf + lerp(v1, v2));
            let b = || (xf + lerp(v3, v2), yf + 1.0);
            let l = || (xf, yf + lerp(v0, v3));
            let mut push = |s: (f32, f32), e: (f32, f32)| {
                if s != e {
                    segments.push((s, e));
                }
            };
            match case {
                1 => push(b(), l()),
                2 => push(r(), b()),
                3 => push(r(), l()),
                4 => push(t(), r()),
                6 => push(t(), b()),
                7 => push(t(), l()),
                8 => push(l(), t()),
                9 => push(b(), t()),
                11 => push(r(), t()),
                12 => push(l(), r()),
                13 => push(b(), r()),
                14 => push(l(), b()),
                // Saddles: split by the cell-center mean.
                5 => {
                    if (v0 + v1 + v2 + v3) * 0.25 > level {
                        push(t(), l());
                        push(b(), r());
                    } else {
                        push(t(), r());
                        push(b(), l());
                    }
                }
                10 => {
                    if (v0 + v1 + v2 + v3) * 0.25 > level {
                        push(r(), t());
                        push(l(), b());
                    } else {
                        push(l(), t());
                        push(r(), b());
                    }
                }
                _ => unreachable!("cases 0 and 15 are filtered above"),
            }
        }
    }
    segments
}

/// Bit-exact hash key for a segment endpoint (endpoints shared between
/// cells are computed identically, so equality is exact).
fn key(p: (f32, f32)) -> (u32, u32) {
    (p.0.to_bits(), p.1.to_bits())
}

/// Chain directed segments head-to-tail into polylines. Open chains are
/// walked from their heads (a start no other segment ends at); whatever
/// remains is a closed ring, emitted with its first point repeated last.
fn chain_segments(segments: Vec<Segment>) -> Vec<Vec<(f32, f32)>> {
    let mut by_start: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    let mut end_keys: HashMap<(u32, u32), usize> = HashMap::new();
    for (i, (s, e)) in segments.iter().enumerate() {
        by_start.entry(key(*s)).or_default().push(i);
        *end_keys.entry(key(*e)).or_default() += 1;
    }
    let mut used = vec![false; segments.len()];
    let walk = |i: usize, used: &mut Vec<bool>| -> Vec<(f32, f32)> {
        used[i] = true;
        let mut line = vec![segments[i].0, segments[i].1];
        while let Some(candidates) = by_start.get(&key(*line.last().expect("non-empty"))) {
            let Some(&j) = candidates.iter().find(|&&j| !used[j]) else {
                break;
            };
            used[j] = true;
            line.push(segments[j].1);
        }
        line
    };
    let mut out = Vec::new();
    // Open chains first, so a ring is never entered mid-way.
    for i in 0..segments.len() {
        if !used[i] && !end_keys.contains_key(&key(segments[i].0)) {
            out.push(walk(i, &mut used));
        }
    }
    // Remaining segments belong to closed rings.
    for i in 0..segments.len() {
        if !used[i] {
            out.push(walk(i, &mut used));
        }
    }
    out
}

pub(super) struct ContourFactory;
impl NodeFactory for ContourFactory {
    fn op_name(&self) -> &'static str {
        "contour"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let field = take_input_ref(fields, "field")?;
        let mut r = InReader::new(fields, ctx, 1);
        let interval = r.number_or("interval", 0.0)?;
        let base = r.number_or("base", 0.0)?;

        let levels = match fields.get("levels") {
            None => None,
            Some(v) => {
                let arr = v.as_array().ok_or_else(|| FactoryError::BadField {
                    field: "levels".into(),
                    msg: "expected an array of numbers".into(),
                })?;
                if arr.is_empty() {
                    return Err(FactoryError::BadField {
                        field: "levels".into(),
                        msg: "expected a non-empty array of numbers".into(),
                    });
                }
                let mut levels = Vec::with_capacity(arr.len());
                for (i, v) in arr.iter().enumerate() {
                    levels.push(r.nested(&format!("levels[{i}]"), v)?);
                }
                Some(levels)
            }
        };
        if levels.is_none() {
            if !fields.contains_key("interval") {
                return Err(FactoryError::MissingField("interval".into()));
            }
            if let In::Const(v) = interval {
                if v <= 0.0 {
                    return Err(FactoryError::BadField {
                        field: "interval".into(),
                        msg: "must be > 0".into(),
                    });
                }
            }
        }
        let read_opt = |name: &str| -> Result<Option<f64>, FactoryError> {
            if fields.contains_key(name) {
                Ok(Some(read_number(fields, name, ctx)?))
            } else {
                Ok(None)
            }
        };
        let min = read_opt("min")?;
        let max = read_opt("max")?;
        let parts = r.finish();

        let mut ports = vec![PortSpec {
            name: "field",
            accepts: &[PortKind::ScalarField],
            optional: false,
        }];
        ports.extend(parts.ports);
        let mut connections = vec![Connection {
            port: "field".into(),
            src: field,
        }];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(ContourNode {
                interval,
                base,
                levels,
                min,
                max,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Isolines from a ScalarField (marching squares with linear sub-cell interpolation), chained into polylines. One feature group per level with properties `{\"level\": <number>}`, so data-driven paint can style by level. Levels are `base + k·interval`, or the explicit `levels` array (which overrides both), clamped by `min`/`max`. Chain `dem → contour → stroke` for elevation contours, or `density → contour → stroke` for point-density isolines.",
            "properties": {
                "field": schema_frag::node_ref(),
                "interval": schema_frag::in_number(serde_json::json!({ "type": "number", "exclusiveMinimum": 0.0,
                              "description": "Spacing between levels, in field units. Required unless `levels` is given." })),
                "base": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 0.0,
                          "description": "Offset the `interval` grid: levels sit at `base + k·interval`." })),
                "levels": { "type": "array", "minItems": 1,
                            "items": schema_frag::nested_number(serde_json::json!({ "type": "number" })),
                            "description": "Explicit levels to extract; overrides `interval`/`base`. A level may be a `$param`; the list is sorted and de-duplicated per render." },
                "min": { "type": "number", "description": "Emit no level below this value." },
                "max": { "type": "number", "description": "Emit no level above this value." },
            },
            "required": ["field"],
        })
    }
}

ezu_graph::submit_node!(ContourFactory);
