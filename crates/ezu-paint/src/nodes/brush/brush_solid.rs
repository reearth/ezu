//! `brush-solid` — `() -> Brush`. Build a hokusai brush that paints a
//! crisp, constant-width line: no scatter, no jitter, dense dabs.
//!
//! Useful as a synthetic alternative to a `.myb` file when the user
//! just wants "draw a line of width N in color C" without authoring a
//! brush.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue,
};
use hokusai::{Brush, BrushSetting};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{read_color, read_number, read_number_or, srgb_to_linear_rgba};

struct BrushSolidNode {
    width_px: f32,
    hsv: (f32, f32, f32),
    hardness: f32,
    aa: f32,
    dabs_per_radius: f32,
}

impl Node for BrushSolidNode {
    fn op_name(&self) -> &'static str {
        "brush-solid"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Brush
    }
    fn eval(&self, _ctx: &EvalCtx<'_>, _: &[Option<PortValue>]) -> Result<PortValue, EvalError> {
        let mut b = Brush::new();
        let radius_px = (self.width_px * 0.5).max(0.2);
        b.get_mut(BrushSetting::Radius).base_value = radius_px.ln();
        b.get_mut(BrushSetting::Opaque).base_value = 1.0;
        b.get_mut(BrushSetting::Hardness).base_value = self.hardness;
        b.get_mut(BrushSetting::AntiAliasing).base_value = self.aa;
        b.get_mut(BrushSetting::DabsPerActualRadius).base_value = self.dabs_per_radius;
        let (h, s, v) = self.hsv;
        b.get_mut(BrushSetting::ColorH).base_value = h;
        b.get_mut(BrushSetting::ColorS).base_value = s;
        b.get_mut(BrushSetting::ColorV).base_value = v;
        Ok(PortValue::Brush(Arc::new(b)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"brush-solid");
        h.update(&self.width_px.to_le_bytes());
        h.update(&self.hsv.0.to_le_bytes());
        h.update(&self.hsv.1.to_le_bytes());
        h.update(&self.hsv.2.to_le_bytes());
        h.update(&self.hardness.to_le_bytes());
        h.update(&self.aa.to_le_bytes());
        h.update(&self.dabs_per_radius.to_le_bytes());
    }
}

pub(super) struct BrushSolidFactory;
impl NodeFactory for BrushSolidFactory {
    fn op_name(&self) -> &'static str {
        "brush-solid"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let width_px = read_number(fields, "width-px", ctx)? as f32;
        let color_srgb = read_color(fields, "color", ctx)?;
        let lin = srgb_to_linear_rgba(color_srgb);
        let hsv = linear_rgb_to_hsv([lin[0], lin[1], lin[2]]);
        let hardness = read_number_or(fields, "hardness", ctx, 1.0)? as f32;
        let aa = read_number_or(fields, "aa", ctx, 1.0)? as f32;
        let dabs_per_radius = read_number_or(fields, "dabs-per-radius", ctx, 4.0)? as f32;
        Ok(BuiltNode {
            node: Box::new(BrushSolidNode {
                width_px,
                hsv,
                hardness: hardness.clamp(0.0, 1.0),
                aa: aa.clamp(0.0, 1.0),
                dabs_per_radius: dabs_per_radius.max(0.5),
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Synthesize a crisp constant-width hokusai brush. No scatter / no jitter — dabs are stacked densely so the stroke reads as a solid line.",
            "properties": {
                "width-px": { "type": "number", "minimum": 0.4,
                              "description": "Stroke width in canvas pixels." },
                "color": schema_frag::color(),
                "hardness": schema_frag::unit_number(),
                "aa": schema_frag::unit_number(),
                "dabs-per-radius": { "type": "number", "minimum": 0.5,
                                     "description": "Dab density along the stroke. Higher = smoother but slower. Default 4." },
            },
            "required": ["width-px", "color"],
        })
    }
}

ezu_graph::submit_node!(BrushSolidFactory);

fn linear_rgb_to_hsv(rgb: [f32; 3]) -> (f32, f32, f32) {
    // hokusai stores color in libmypaint HSV (each component in [0, 1]).
    // Convert the linear-sRGB triple back to gamma sRGB first so the
    // resulting HSV matches what an artist sees in libmypaint.
    let r = linear_to_srgb(rgb[0]);
    let g = linear_to_srgb(rgb[1]);
    let b = linear_to_srgb(rgb[2]);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let v = max;
    let s = if max > 0.0 { d / max } else { 0.0 };
    let h = if d <= 1e-6 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    (h, s.clamp(0.0, 1.0), v.clamp(0.0, 1.0))
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}
