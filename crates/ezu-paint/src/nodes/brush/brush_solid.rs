//! `brush-solid` — `() -> Brush`. Build a hokusai brush that paints a
//! crisp, constant-width line: no scatter, no jitter, dense dabs.
//!
//! Useful as a synthetic alternative to a `.myb` file when the user
//! just wants "draw a line of width N in color C" without authoring a
//! brush.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, In, InReader, InkReach,
    Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use hokusai::{Brush, BrushSetting};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::srgb_to_linear_rgba;

struct BrushSolidNode {
    width_px: In<f64>,
    color: In<[f32; 4]>,
    hardness: In<f64>,
    aa: In<f64>,
    dabs_per_radius: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for BrushSolidNode {
    fn op_name(&self) -> &'static str {
        "brush-solid"
    }

    fn ink_reach(&self, _assets: &dyn ezu_graph::AssetLoader) -> Option<InkReach> {
        // A solid brush is a plain round dab, so its reach follows from
        // its width alone — as long as that width has a ceiling.
        let radius_px = (self.width_px.static_bound()? * 0.5).max(0.2) as f32;
        let mut b = Brush::new();
        b.get_mut(BrushSetting::Radius).base_value = radius_px.ln();
        Some(InkReach {
            reach_px: crate::strokes::max_dab_reach_px(&b) as f64,
            radius_px: radius_px as f64,
        })
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Brush
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let width_px = self.width_px.get(ctx, inputs)? as f32;
        let lin = srgb_to_linear_rgba(self.color.get(ctx, inputs)?);
        let (h_col, s_col, v_col) = linear_rgb_to_hsv([lin[0], lin[1], lin[2]]);
        let hardness = (self.hardness.get(ctx, inputs)? as f32).clamp(0.0, 1.0);
        let aa = (self.aa.get(ctx, inputs)? as f32).clamp(0.0, 1.0);
        let dabs_per_radius = (self.dabs_per_radius.get(ctx, inputs)? as f32).max(0.5);

        let mut b = Brush::new();
        let radius_px = (width_px * 0.5).max(0.2);
        b.get_mut(BrushSetting::Radius).base_value = radius_px.ln();
        b.get_mut(BrushSetting::Opaque).base_value = 1.0;
        b.get_mut(BrushSetting::Hardness).base_value = hardness;
        b.get_mut(BrushSetting::AntiAliasing).base_value = aa;
        b.get_mut(BrushSetting::DabsPerActualRadius).base_value = dabs_per_radius;
        b.get_mut(BrushSetting::ColorH).base_value = h_col;
        b.get_mut(BrushSetting::ColorS).base_value = s_col;
        b.get_mut(BrushSetting::ColorV).base_value = v_col;
        Ok(PortValue::Brush(Arc::new(b)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"brush-solid");
        self.width_px.param_hash(h);
        self.color.param_hash(h);
        self.hardness.param_hash(h);
        self.aa.param_hash(h);
        self.dabs_per_radius.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        let mut r = InReader::new(fields, ctx, 0);
        let width_px = r.number("width-px")?;
        let color = r.color("color")?;
        let hardness = r.number_or("hardness", 1.0)?;
        let aa = r.number_or("aa", 1.0)?;
        let dabs_per_radius = r.number_or("dabs-per-radius", 4.0)?;
        let parts = r.finish();
        Ok(BuiltNode {
            node: Box::new(BrushSolidNode {
                width_px,
                color,
                hardness,
                aa,
                dabs_per_radius,
                ports: parts.ports,
                param_refs: parts.param_refs,
            }),
            connections: parts.connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Synthesize a crisp constant-width hokusai brush. No scatter / no jitter — dabs are stacked densely so the stroke reads as a solid line.",
            "properties": {
                "width-px": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.4,
                              "description": "Stroke width in canvas pixels." })),
                "color": schema_frag::color(),
                "hardness": schema_frag::unit_number(),
                "aa": schema_frag::unit_number(),
                "dabs-per-radius": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.5,
                                     "description": "Dab density along the stroke. Higher = smoother but slower. Default 4." })),
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
