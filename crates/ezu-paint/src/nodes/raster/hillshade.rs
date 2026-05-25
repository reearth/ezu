//! `hillshade` — `HeightField -> Raster`. Classic Horn-method analytical
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
    FactoryError, HeightField, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use super::terrain_common::horn_gradient;
use crate::nodes::common::{read_number_or, read_string_or};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Shade,
    Relief,
}

struct HillshadeNode {
    azimuth_deg: f32,
    altitude_deg: f32,
    z_factor: f32,
    exaggeration: f32,
    multidirectional: bool,
    mode: OutputMode,
}

impl Node for HillshadeNode {
    fn op_name(&self) -> &'static str {
        "hillshade"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "field",
            accepts: &[PortKind::HeightField],
            optional: false,
        }];
        SPECS
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let field = inputs[0]
            .as_ref()
            .and_then(PortValue::as_height_field)
            .ok_or_else(|| EvalError::MissingInput("field".into()))?;
        let out = if self.multidirectional {
            self.render_multidirectional(field)
        } else {
            self.render_single(field)
        };
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"hillshade");
        h.update(&self.azimuth_deg.to_le_bytes());
        h.update(&self.altitude_deg.to_le_bytes());
        h.update(&self.z_factor.to_le_bytes());
        h.update(&self.exaggeration.to_le_bytes());
        h.update(&[self.multidirectional as u8]);
        h.update(match self.mode {
            OutputMode::Shade => b"sh",
            OutputMode::Relief => b"rl",
        });
    }
}

impl HillshadeNode {
    fn render_single(&self, field: &HeightField) -> RasterBuf {
        let azimuth_rad = (450.0 - self.azimuth_deg).to_radians();
        let altitude_rad = self.altitude_deg.to_radians();
        let cos_zenith = (std::f32::consts::FRAC_PI_2 - altitude_rad).cos();
        let sin_zenith = (std::f32::consts::FRAC_PI_2 - altitude_rad).sin();
        let scale = self.z_factor * self.exaggeration;
        self.render_with(field, |dx, dy| {
            shade_sample(dx, dy, scale, cos_zenith, sin_zenith, azimuth_rad)
        })
    }

    fn render_multidirectional(&self, field: &HeightField) -> RasterBuf {
        // ESRI-style weighted sum over four light directions
        // (225°, 270°, 315°, 360°). Weights from the published recipe.
        let altitudes_rad = self.altitude_deg.to_radians();
        let cos_zenith = (std::f32::consts::FRAC_PI_2 - altitudes_rad).cos();
        let sin_zenith = (std::f32::consts::FRAC_PI_2 - altitudes_rad).sin();
        let scale = self.z_factor * self.exaggeration;
        let dirs = [(225.0f32, 1.0), (270.0, 2.0), (315.0, 2.0), (360.0, 1.0)];
        let weight_sum: f32 = dirs.iter().map(|(_, w)| *w).sum();
        let azimuths: Vec<(f32, f32)> = dirs
            .iter()
            .map(|(az, w)| ((450.0 - az).to_radians(), w / weight_sum))
            .collect();
        self.render_with(field, |dx, dy| {
            azimuths
                .iter()
                .map(|(az, w)| w * shade_sample(dx, dy, scale, cos_zenith, sin_zenith, *az))
                .sum()
        })
    }

    fn render_with(&self, field: &HeightField, sample: impl Fn(f32, f32) -> f32) -> RasterBuf {
        let w = field.width;
        let h = field.height;
        let mut out = RasterBuf::new(w, h);
        // 8 * pitch is the Horn-method divisor for the central
        // differences below; bake it in once.
        let inv_x = 1.0 / (8.0 * field.metres_per_pixel_x.max(1e-6));
        let inv_y = 1.0 / (8.0 * field.metres_per_pixel_y.max(1e-6));
        for y in 0..h {
            for x in 0..w {
                let (dz_dx, dz_dy) = horn_gradient(field, x, y, inv_x, inv_y);
                let shade = sample(dz_dx, dz_dy).clamp(0.0, 1.0);
                let i = ((y * w + x) * 4) as usize;
                match self.mode {
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
        let azimuth_deg = read_number_or(fields, "azimuth-deg", ctx, 315.0)? as f32;
        let altitude_deg = read_number_or(fields, "altitude-deg", ctx, 45.0)? as f32;
        let z_factor = read_number_or(fields, "z-factor", ctx, 1.0)? as f32;
        let exaggeration = read_number_or(fields, "exaggeration", ctx, 1.0)? as f32;
        let multidirectional = fields
            .get("multidirectional")
            .and_then(Value::as_bool)
            .unwrap_or(false);
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
        Ok(BuiltNode {
            node: Box::new(HillshadeNode {
                azimuth_deg,
                altitude_deg,
                z_factor,
                exaggeration,
                multidirectional,
                mode,
            }),
            connections: vec![Connection {
                port: "field".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Analytical hillshade (Horn 1981) from a HeightField. `mode: shade` outputs grayscale; `mode: relief` outputs transparent black scaled by 1-shade for multiply-blend over a base map.",
            "properties": {
                "field": schema_frag::node_ref(),
                "azimuth-deg": { "type": "number", "default": 315,
                                 "description": "Light direction (0 = north, clockwise)." },
                "altitude-deg": { "type": "number", "default": 45,
                                  "description": "Light elevation above the horizon (degrees)." },
                "z-factor": { "type": "number", "default": 1.0,
                              "description": "Multiplier on elevation before gradient — use ~1/cos(lat) for high latitudes." },
                "exaggeration": { "type": "number", "default": 1.0,
                                  "description": "Extra vertical exaggeration on top of z-factor." },
                "multidirectional": { "type": "boolean", "default": false,
                                       "description": "ESRI-style 4-direction weighted hillshade — softer, more legible at small zooms." },
                "mode": { "type": "string", "enum": ["shade", "relief"], "default": "shade" },
            },
            "required": ["field"],
        })
    }
}

ezu_graph::submit_node!(HillshadeFactory);
