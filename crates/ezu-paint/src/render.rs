//! Render a parsed Ezu Style [`Style`] onto a [`Canvas`].
//!
//! Translates each [`LayerSpec`] into the matching `ezu-paint` call.
//! Brush references (`@name` or path) are resolved by the caller through
//! a [`BrushResolver`] closure.

use std::collections::HashMap;

use ezu_core::TileId;
use ezu_mvt::{DecodedTile, Feature, Geometry, Polygon, Value};
use ezu_style::{
    FeatureFilter, FillDabsSpec, FillSolidSpec, FilterAtom, FilterMatch, HexColor, LayerSpec,
    LineSpec, Style,
};
use hokusai::Brush;
use tiny_skia::Color;

use crate::{
    paint_lines, paint_polygons, paint_polygons_dabs, Canvas, DabFillStyle, LineStrokeStyle,
    RgbaF32, WatercolorStyle,
};

/// Closure type for resolving brush references at render time.
///
/// `@name` references are usually pre-loaded into a `HashMap<String, Brush>`;
/// other references can be treated as paths and parsed via
/// `hokusai::myb::from_str`.
pub type BrushResolver<'a> = &'a dyn Fn(&str) -> Option<&'a Brush>;

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("brush not found: {0}")]
    BrushMissing(String),
}

/// Build a [`Canvas`] sized by the style (`tile_size + 2 * pad`) and prefill
/// the paper background.
pub fn canvas_from_style(style: &Style) -> Canvas {
    canvas_from_style_sized(style, style.tile_size, style.pad)
}

/// Like [`canvas_from_style`] but with `tile_size` and `pad` overridden —
/// useful for hi-DPI / preview renders without mutating the style.
pub fn canvas_from_style_sized(style: &Style, tile_size: u32, pad: u32) -> Canvas {
    let mut canvas = Canvas::new_padded(tile_size, tile_size, pad);
    canvas.fill(hex_to_tinyskia(style.background));
    canvas
}

/// Apply every layer in `style` to `canvas`, drawing features from `decoded`.
///
/// When `trace` is true, prints per-layer wallclock to stderr.
pub fn render_style(
    canvas: &mut Canvas,
    style: &Style,
    decoded: &DecodedTile,
    tile: TileId,
    resolve_brush: BrushResolver<'_>,
) -> Result<(), RenderError> {
    // Tracing relies on `Instant::now()`, which panics on
    // `wasm32-unknown-unknown` (no monotonic clock). Gate everything on the
    // env flag so the timing call is not even materialized on WASM.
    let trace = trace_enabled();
    for layer in &style.layers {
        let t = trace.then(std::time::Instant::now);
        let (kind, id) = match layer {
            LayerSpec::FillSolid(spec) => {
                apply_fill_solid(canvas, decoded, tile, spec);
                ("fill-solid", spec.id.as_str())
            }
            LayerSpec::FillDabs(spec) => {
                apply_fill_dabs(canvas, decoded, tile, spec);
                ("fill-dabs", spec.id.as_str())
            }
            LayerSpec::Line(spec) => {
                apply_line(canvas, decoded, tile, spec, resolve_brush)?;
                ("line", spec.id.as_str())
            }
        };
        if let Some(t) = t {
            eprintln!(
                "    [{:>10}] {:<10} {:>6.1}ms",
                kind,
                id,
                t.elapsed().as_secs_f64() * 1000.0,
            );
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn trace_enabled() -> bool {
    std::env::var_os("EZU_TRACE").is_some()
}

#[cfg(target_arch = "wasm32")]
fn trace_enabled() -> bool {
    false
}

fn apply_fill_solid(
    canvas: &mut Canvas,
    decoded: &DecodedTile,
    tile: TileId,
    spec: &FillSolidSpec,
) {
    let Some(layer) = decoded.layer(&spec.source_layer) else {
        return;
    };
    let polys = collect_polygons(&layer.features, &spec.filter, &spec.min_zoom_field, tile.z);
    if polys.is_empty() {
        return;
    }
    let style = WatercolorStyle {
        fill: tint_alpha(spec.fill, spec.fill_alpha),
        edge: spec.edge.map(hex_to_tinyskia),
        edge_width: spec.edge_width,
        blur_sigma: spec.blur_sigma,
    };
    paint_polygons(canvas, &polys, layer.extent, &style);
}

fn apply_fill_dabs(canvas: &mut Canvas, decoded: &DecodedTile, tile: TileId, spec: &FillDabsSpec) {
    let Some(layer) = decoded.layer(&spec.source_layer) else {
        return;
    };
    let polys = collect_polygons(&layer.features, &spec.filter, &spec.min_zoom_field, tile.z);
    if polys.is_empty() {
        return;
    }
    let [r, g, b] = spec.color.srgb_linear();
    let style = DabFillStyle {
        color: RgbaF32::new(r, g, b, 1.0),
        opacity: spec.opacity,
        radius_px: spec.radius_px,
        hardness: spec.hardness,
        paint: spec.paint,
        spacing_px: spec.spacing_px,
        position_jitter: spec.position_jitter,
        size_jitter: spec.size_jitter,
        opacity_jitter: spec.opacity_jitter,
        value_jitter: spec.value_jitter,
    };
    paint_polygons_dabs(canvas, &polys, layer.extent, tile, &style);
}

fn apply_line(
    canvas: &mut Canvas,
    decoded: &DecodedTile,
    tile: TileId,
    spec: &LineSpec,
    resolve_brush: BrushResolver<'_>,
) -> Result<(), RenderError> {
    let Some(layer) = decoded.layer(&spec.source_layer) else {
        return Ok(());
    };
    let lines = collect_lines(&layer.features, &spec.filter, &spec.min_zoom_field, tile.z);
    if lines.is_empty() {
        return Ok(());
    }
    let brush =
        resolve_brush(&spec.brush).ok_or_else(|| RenderError::BrushMissing(spec.brush.clone()))?;
    // Apply optional radius / opacity overrides via a local brush clone.
    let mut brush_local;
    let brush_ref: &Brush = if spec.radius_px.is_some() || spec.opacity.is_some() {
        brush_local = brush.clone();
        if let Some(r) = spec.radius_px {
            brush_local
                .get_mut(hokusai::BrushSetting::Radius)
                .base_value = r.max(0.05).ln();
        }
        if let Some(o) = spec.opacity {
            brush_local
                .get_mut(hokusai::BrushSetting::Opaque)
                .base_value = o.clamp(0.0, 1.0);
        }
        &brush_local
    } else {
        brush
    };
    let style = LineStrokeStyle {
        color: spec.color.srgb_linear(),
        pressure_base: spec.pressure_base,
        pressure_jitter: spec.pressure_jitter,
        dtime: spec.dtime,
    };
    paint_lines(canvas, &lines, layer.extent, tile, brush_ref, &style);
    Ok(())
}

fn collect_polygons(
    features: &[Feature],
    filter: &Option<FeatureFilter>,
    min_zoom_field: &Option<String>,
    z: u8,
) -> Vec<Polygon> {
    let mut out = Vec::new();
    for f in features {
        if !feature_passes(f, filter, min_zoom_field, z) {
            continue;
        }
        if let Geometry::Polygons(ps) = &f.geometry {
            for p in ps {
                out.push(Polygon {
                    exterior: p.exterior.clone(),
                    holes: p.holes.clone(),
                });
            }
        }
    }
    out
}

fn collect_lines(
    features: &[Feature],
    filter: &Option<FeatureFilter>,
    min_zoom_field: &Option<String>,
    z: u8,
) -> Vec<Vec<(i32, i32)>> {
    let mut out = Vec::new();
    for f in features {
        if !feature_passes(f, filter, min_zoom_field, z) {
            continue;
        }
        if let Geometry::Lines(ls) = &f.geometry {
            out.extend(ls.iter().cloned());
        }
    }
    out
}

fn feature_passes(
    f: &Feature,
    filter: &Option<FeatureFilter>,
    min_zoom_field: &Option<String>,
    z: u8,
) -> bool {
    if let Some(field) = min_zoom_field.as_ref() {
        let min_zoom_ok = f
            .properties
            .get(field)
            .and_then(value_as_i64)
            .map(|mz| mz <= z as i64)
            .unwrap_or(true); // missing field → assume visible
        if !min_zoom_ok {
            return false;
        }
    }
    if let Some(filter) = filter.as_ref() {
        for (k, expected) in filter {
            let Some(actual) = f.properties.get(k) else {
                return false;
            };
            if !match_value(actual, expected) {
                return false;
            }
        }
    }
    true
}

fn match_value(actual: &Value, expected: &FilterMatch) -> bool {
    match expected {
        FilterMatch::One(atom) => atom_equals(actual, atom),
        FilterMatch::Any(atoms) => atoms.iter().any(|a| atom_equals(actual, a)),
    }
}

fn atom_equals(actual: &Value, expected: &FilterAtom) -> bool {
    match (actual, expected) {
        (Value::String(a), FilterAtom::Str(b)) => a == b,
        (Value::Bool(a), FilterAtom::Bool(b)) => a == b,
        (Value::Int(a), FilterAtom::Int(b)) | (Value::SInt(a), FilterAtom::Int(b)) => a == b,
        (Value::UInt(a), FilterAtom::Int(b)) => (*a as i64) == *b,
        (Value::Float(a), FilterAtom::Float(b)) => (*a as f64) == *b,
        (Value::Double(a), FilterAtom::Float(b)) => a == b,
        (Value::Int(a), FilterAtom::Float(b)) | (Value::SInt(a), FilterAtom::Float(b)) => {
            (*a as f64) == *b
        }
        _ => false,
    }
}

fn value_as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Int(n) | Value::SInt(n) => Some(*n),
        Value::UInt(n) => Some(*n as i64),
        Value::Float(n) => Some(*n as i64),
        Value::Double(n) => Some(*n as i64),
        _ => None,
    }
}

fn hex_to_tinyskia(c: HexColor) -> Color {
    Color::from_rgba8(c.r, c.g, c.b, c.a)
}

fn tint_alpha(c: HexColor, alpha_mul: f32) -> Color {
    let a = ((c.a as f32) * alpha_mul.clamp(0.0, 1.0)).round() as u8;
    Color::from_rgba8(c.r, c.g, c.b, a)
}

/// Build a `HashMap<String, Brush>` from `(name, brush)` pairs. Convenience
/// helper for the common case where all brushes are pre-loaded.
pub fn brush_bank<I>(entries: I) -> HashMap<String, Brush>
where
    I: IntoIterator<Item = (String, Brush)>,
{
    entries.into_iter().collect()
}
