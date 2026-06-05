//! `hillshade` — `ScalarField -> Raster`. Classic Horn-method analytical
//! hillshade: a single illumination direction (azimuth + altitude) lit
//! against per-pixel slope and aspect derived from a 3×3 gradient
//! window on the input elevations.
//!
//! Two output modes:
//!
//! - `shade` (default) — grayscale RGBA (`(g, g, g, 1)`), suitable as a
//!   standalone layer or to be tinted via `hsl`.
//! - `relief` — transparent black scaled by `1 - shade`, designed for
//!   compositing over a base map with `blend`'s `multiply` /
//!   `source-over`.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
    ScalarField,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use super::terrain_common::horn_gradient;
use crate::nodes::common::read_string_or;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Shade,
    Relief,
}

struct HillshadeNode {
    azimuth_deg: In<f64>,
    altitude_deg: In<f64>,
    z_factor: In<f64>,
    exaggeration: In<f64>,
    multidirectional: In<bool>,
    mode: OutputMode,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for HillshadeNode {
    fn op_name(&self) -> &'static str {
        "hillshade"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
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
        let azimuth_deg = self.azimuth_deg.get(ctx, inputs)? as f32;
        let altitude_deg = self.altitude_deg.get(ctx, inputs)? as f32;
        let z_factor = self.z_factor.get(ctx, inputs)? as f32;
        let exaggeration = self.exaggeration.get(ctx, inputs)? as f32;
        let out = if self.multidirectional.get(ctx, inputs)? {
            render_multidirectional(field, altitude_deg, z_factor, exaggeration, self.mode)
        } else {
            render_single(
                field,
                azimuth_deg,
                altitude_deg,
                z_factor,
                exaggeration,
                self.mode,
            )
        };
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"hillshade");
        self.azimuth_deg.param_hash(h);
        self.altitude_deg.param_hash(h);
        self.z_factor.param_hash(h);
        self.exaggeration.param_hash(h);
        self.multidirectional.param_hash(h);
        h.update(match self.mode {
            OutputMode::Shade => b"sh",
            OutputMode::Relief => b"rl",
        });
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

fn render_single(
    field: &ScalarField,
    azimuth_deg: f32,
    altitude_deg: f32,
    z_factor: f32,
    exaggeration: f32,
    mode: OutputMode,
) -> RasterBuf {
    let azimuth_rad = (450.0 - azimuth_deg).to_radians();
    let altitude_rad = altitude_deg.to_radians();
    let cos_zenith = (std::f32::consts::FRAC_PI_2 - altitude_rad).cos();
    let sin_zenith = (std::f32::consts::FRAC_PI_2 - altitude_rad).sin();
    let scale = z_factor * exaggeration;
    render_with(field, mode, |dx, dy| {
        shade_sample(dx, dy, scale, cos_zenith, sin_zenith, azimuth_rad)
    })
}

fn render_multidirectional(
    field: &ScalarField,
    altitude_deg: f32,
    z_factor: f32,
    exaggeration: f32,
    mode: OutputMode,
) -> RasterBuf {
    // ESRI-style weighted sum over four light directions
    // (225°, 270°, 315°, 360°). Weights from the published recipe.
    let altitudes_rad = altitude_deg.to_radians();
    let cos_zenith = (std::f32::consts::FRAC_PI_2 - altitudes_rad).cos();
    let sin_zenith = (std::f32::consts::FRAC_PI_2 - altitudes_rad).sin();
    let scale = z_factor * exaggeration;
    let dirs = [(225.0f32, 1.0), (270.0, 2.0), (315.0, 2.0), (360.0, 1.0)];
    let weight_sum: f32 = dirs.iter().map(|(_, w)| *w).sum();
    let azimuths: Vec<(f32, f32)> = dirs
        .iter()
        .map(|(az, w)| ((450.0 - az).to_radians(), w / weight_sum))
        .collect();
    render_with(field, mode, |dx, dy| {
        azimuths
            .iter()
            .map(|(az, w)| w * shade_sample(dx, dy, scale, cos_zenith, sin_zenith, *az))
            .sum()
    })
}

fn render_with(
    field: &ScalarField,
    mode: OutputMode,
    sample: impl Fn(f32, f32) -> f32,
) -> RasterBuf {
    let w = field.width;
    let h = field.height;
    let mut out = RasterBuf::new(w, h);
    // 8 * pitch is the Horn-method divisor for the central
    // differences below; bake it in once.
    let inv_x = 1.0 / (8.0 * field.metres_per_pixel_x().max(1e-6));
    let inv_y = 1.0 / (8.0 * field.metres_per_pixel_y().max(1e-6));
    for y in 0..h {
        for x in 0..w {
            let (dz_dx, dz_dy) = horn_gradient(field, x, y, inv_x, inv_y);
            let shade = sample(dz_dx, dz_dy).clamp(0.0, 1.0);
            let i = ((y * w + x) * 4) as usize;
            match mode {
                OutputMode::Shade => {
                    let g = (shade * 255.0).round() as u8;
                    out.pixels[i] = g;
                    out.pixels[i + 1] = g;
                    out.pixels[i + 2] = g;
                    out.pixels[i + 3] = 255;
                }
                OutputMode::Relief => {
                    // Premultiplied transparent black at alpha = 1 - shade.
                    let a = ((1.0 - shade) * 255.0).round() as u8;
                    out.pixels[i] = 0;
                    out.pixels[i + 1] = 0;
                    out.pixels[i + 2] = 0;
                    out.pixels[i + 3] = a;
                }
            }
        }
    }
    out
}

#[inline]
fn shade_sample(
    dz_dx: f32,
    dz_dy: f32,
    scale: f32,
    cos_zenith: f32,
    sin_zenith: f32,
    azimuth_rad: f32,
) -> f32 {
    let dx = dz_dx * scale;
    let dy = dz_dy * scale;
    let slope = (dx * dx + dy * dy).sqrt().atan();
    // Aspect: angle the slope faces, measured clockwise from east in
    // mathematical convention (matches `azimuth_rad` after the 450°
    // shift applied by the caller).
    let aspect = dy.atan2(-dx);
    cos_zenith * slope.cos() + sin_zenith * slope.sin() * (azimuth_rad - aspect).cos()
}

pub(super) struct HillshadeFactory;
impl NodeFactory for HillshadeFactory {
    fn op_name(&self) -> &'static str {
        "hillshade"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "field")?;
        let mode = match read_string_or(fields, "mode", ctx, "shade")?.as_str() {
            "shade" => OutputMode::Shade,
            "relief" => OutputMode::Relief,
            other => {
                return Err(FactoryError::BadField {
                    field: "mode".into(),
                    msg: format!("expected `shade` or `relief`, got `{other}`"),
                });
            }
        };
        let mut r = InReader::new(fields, ctx, 1);
        let azimuth_deg = r.number_or("azimuth-deg", 315.0)?;
        let altitude_deg = r.number_or("altitude-deg", 45.0)?;
        let z_factor = r.number_or("z-factor", 1.0)?;
        let exaggeration = r.number_or("exaggeration", 1.0)?;
        let multidirectional = r.bool_or("multidirectional", false)?;
        let parts = r.finish();

        let mut ports = vec![PortSpec {
            name: "field",
            accepts: &[PortKind::ScalarField],
            optional: false,
        }];
        ports.extend(parts.ports);
        let mut connections = vec![Connection {
            port: "field".into(),
            src: input,
        }];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(HillshadeNode {
                azimuth_deg,
                altitude_deg,
                z_factor,
                exaggeration,
                multidirectional,
                mode,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Analytical hillshade (Horn 1981) from a ScalarField. `mode: shade` outputs grayscale; `mode: relief` outputs transparent black scaled by 1-shade for multiply-blend over a base map.",
            "properties": {
                "field": schema_frag::node_ref(),
                "azimuth-deg": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 315,
                                 "description": "Light direction (0 = north, clockwise)." })),
                "altitude-deg": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 45,
                                  "description": "Light elevation above the horizon (degrees)." })),
                "z-factor": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 1.0,
                              "description": "Multiplier on elevation before gradient — use ~1/cos(lat) for high latitudes." })),
                "exaggeration": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 1.0,
                                  "description": "Extra vertical exaggeration on top of z-factor." })),
                "multidirectional": { "oneOf": [{"type": "boolean"}, {"type": "string", "pattern": "^[$@].+"}], "default": false,
                                       "description": "ESRI-style 4-direction weighted hillshade — softer, more legible at small zooms." },
                "mode": { "type": "string", "enum": ["shade", "relief"], "default": "shade" },
            },
            "required": ["field"],
        })
    }
}

ezu_graph::submit_node!(HillshadeFactory);
