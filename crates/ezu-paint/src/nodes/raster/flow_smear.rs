//! `flow-smear` — line integral convolution of `Raster|Sprite` (the
//! `input` is pass-through) along a direction raster (`field`). Each
//! output pixel averages `input` along the streamline through it, so a
//! speckled texture turns into short strokes that follow the field.
//!
//! The field uses `flow-field`'s (and `displace`'s) encoding: R/G are
//! `0.5 + 0.5·v` in pixel axes, flat grey meaning no direction. The walk
//! re-reads the field at every 1 px step, so strokes bend with it rather
//! than running off in a straight line, and a direction that points back
//! against the previous step is flipped so a stroke never folds onto
//! itself. It stops where the field goes flat. A pixel's walk is
//! `length-px` times the field's length at that pixel, which is how
//! gentle ground gets short strokes and steep ground long ones.
//!
//! `sides: both` walks both ways from the pixel. `sides: forward` walks
//! only upstream, against the field: a pixel then gathers what lies
//! behind it, so a bright speck in `input` trails downstream, the way
//! the field points.
//!
//! The usual chain is `dem → flow-field → flow-smear` over a `noise`
//! raster, then `threshold` or `color-ramp` to turn the streaks into
//! ink: hachures with `direction: downhill`, strokes along the contours
//! with `along`.
//!
//! Seamless across tile borders when `input` is world-anchored (`noise`
//! with `anchor: world`, its default) and the field comes from a DEM or
//! another world-anchored source: the walk is computed relative to each
//! pixel, and the upstream pad grows by `length-px` + 1 so it never runs
//! off the available raster.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, InReader, Node, NodeFactory, PaddingIn, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    raster_or_sprite_output, read_string_or, unwrap_raster_or_sprite, wrap_raster_like,
    ACCEPTS_RASTER_OR_SPRITE,
};

/// Field length below which a pixel counts as flat. Two 8-bit steps from
/// the 0.5 midpoint: the encoding of a zero vector rounds to 128, a hair
/// off centre, and that must still read as "no direction".
const FLAT: f32 = 2.0 / 127.5;

/// Samples in the Hann taper table, over the walk's `[0, 1]`.
const HANN_STEPS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sides {
    Both,
    Forward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Taper {
    Flat,
    Smooth,
}

struct FlowSmearNode {
    length: PaddingIn,
    sides: Sides,
    taper: Taper,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for FlowSmearNode {
    fn op_name(&self) -> &'static str {
        "flow-smear"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        raster_or_sprite_output(input_kinds)
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        // The walk reaches `length` px out; the bilinear read at its end
        // touches one pixel further.
        downstream + self.length.bound().max(0.0).ceil() as u32 + 1
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
        let field = inputs[1]
            .as_ref()
            .and_then(PortValue::as_raster)
            .ok_or_else(|| EvalError::MissingInput("field".into()))?;
        let length = self.length.get(ctx, inputs)?.max(0.0) as f32;
        if length < 1.0 || field.width == 0 || field.height == 0 {
            // No walk reaches a whole step.
            return Ok(wrap_raster_like(src, kind));
        }
        let hann = match self.taper {
            Taper::Flat => None,
            Taper::Smooth => Some(hann_table()),
        };
        let (w, h) = (src.width, src.height);
        let mut out = RasterBuf::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (bx, by) = (x as i64, y as i64);
                let centre = src.pixel(x, y);
                let mut acc = [
                    centre[0] as f32,
                    centre[1] as f32,
                    centre[2] as f32,
                    centre[3] as f32,
                ];
                let mut weight_sum = 1.0f32;
                let (vx, vy) = read_field(field, bx, by, 0.0, 0.0);
                let mag = (vx * vx + vy * vy).sqrt();
                if mag >= FLAT {
                    let walk = Walk {
                        src: &src,
                        field,
                        bx,
                        by,
                        len: length * mag.min(1.0),
                        hann,
                    };
                    let (dx, dy) = (vx / mag, vy / mag);
                    // Gathering from upstream is what carries the input
                    // downstream, so `forward` walks against the field.
                    walk.run(-dx, -dy, &mut acc, &mut weight_sum);
                    if self.sides == Sides::Both {
                        walk.run(dx, dy, &mut acc, &mut weight_sum);
                    }
                }
                let i = ((y * w + x) * 4) as usize;
                for (o, a) in out.pixels[i..i + 4].iter_mut().zip(acc) {
                    *o = (a / weight_sum).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"flow-smear");
        self.length.param_hash(h);
        h.update(match self.sides {
            Sides::Both => b"bo",
            Sides::Forward => b"fw",
        });
        h.update(match self.taper {
            Taper::Flat => b"fl",
            Taper::Smooth => b"sm",
        });
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

/// One pixel's streamline: everything a walk in either direction shares.
struct Walk<'a> {
    src: &'a RasterBuf,
    field: &'a RasterBuf,
    bx: i64,
    by: i64,
    /// Arc length to cover, in px.
    len: f32,
    hann: Option<&'static [f32; HANN_STEPS + 1]>,
}

impl Walk<'_> {
    /// Step from the pixel in 1 px increments, starting along
    /// `(dx, dy)`, adding each sample of `input` to `acc`.
    ///
    /// The position is kept as an offset from the pixel rather than as
    /// a canvas coordinate, so the arithmetic, and therefore the bytes,
    /// are the same wherever the pixel sits — which is what lets two
    /// tiles agree where they overlap.
    fn run(&self, dx: f32, dy: f32, acc: &mut [f32; 4], weight_sum: &mut f32) {
        let steps = self.len as u32;
        let (mut ox, mut oy) = (0.0f32, 0.0f32);
        let (mut px, mut py) = (dx, dy);
        for k in 1..=steps {
            let (vx, vy) = read_field(self.field, self.bx, self.by, ox, oy);
            let mag = (vx * vx + vy * vy).sqrt();
            if mag < FLAT {
                break;
            }
            let (mut sx, mut sy) = (vx / mag, vy / mag);
            if sx * px + sy * py < 0.0 {
                sx = -sx;
                sy = -sy;
            }
            ox += sx;
            oy += sy;
            px = sx;
            py = sy;
            let wgt = match self.hann {
                None => 1.0,
                Some(table) => hann_at(table, k as f32 / self.len),
            };
            let s = sample(self.src, self.bx, self.by, ox, oy);
            for c in 0..4 {
                acc[c] += wgt * s[c];
            }
            *weight_sum += wgt;
        }
    }
}

/// The field's vector at offset `(ox, oy)` from pixel `(bx, by)`,
/// decoded from its R/G channels.
#[inline]
fn read_field(field: &RasterBuf, bx: i64, by: i64, ox: f32, oy: f32) -> (f32, f32) {
    let s = sample(field, bx, by, ox, oy);
    (s[0] / 127.5 - 1.0, s[1] / 127.5 - 1.0)
}

/// Bilinear sample of a premultiplied RGBA8 raster at pixel `(bx, by)`
/// plus a sub-pixel offset, clamped to the raster's edge and left
/// unrounded for averaging. Splitting the offset into whole and
/// fractional parts here, rather than adding it to the pixel's canvas
/// coordinate first, keeps the weights independent of where the pixel
/// is.
#[inline]
fn sample(src: &RasterBuf, bx: i64, by: i64, ox: f32, oy: f32) -> [f32; 4] {
    let fx = ox.floor();
    let fy = oy.floor();
    let tx = ox - fx;
    let ty = oy - fy;
    let ix = bx + fx as i64;
    let iy = by + fy as i64;
    let max_x = src.width as i64 - 1;
    let max_y = src.height as i64 - 1;
    let x0 = ix.clamp(0, max_x) as u32;
    let x1 = (ix + 1).clamp(0, max_x) as u32;
    let y0 = iy.clamp(0, max_y) as u32;
    let y1 = (iy + 1).clamp(0, max_y) as u32;
    let p00 = src.pixel(x0, y0);
    let p10 = src.pixel(x1, y0);
    let p01 = src.pixel(x0, y1);
    let p11 = src.pixel(x1, y1);
    let mut out = [0.0f32; 4];
    for c in 0..4 {
        let (a00, a10, a01, a11) = (p00[c] as f32, p10[c] as f32, p01[c] as f32, p11[c] as f32);
        let a = a00 + (a10 - a00) * tx;
        let b = a01 + (a11 - a01) * tx;
        out[c] = a + (b - a) * ty;
    }
    out
}

/// The Hann window `0.5·(1 + cos(π·t))` over `t ∈ [0, 1]`, tabulated
/// once so the per-sample weight is a lookup rather than a `cos`.
fn hann_table() -> &'static [f32; HANN_STEPS + 1] {
    static TABLE: std::sync::OnceLock<[f32; HANN_STEPS + 1]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        std::array::from_fn(|i| {
            let t = i as f32 / HANN_STEPS as f32;
            0.5 * (1.0 + libm::cosf(std::f32::consts::PI * t))
        })
    })
}

/// The tabulated Hann weight at `t`, linearly interpolated.
#[inline]
fn hann_at(table: &[f32; HANN_STEPS + 1], t: f32) -> f32 {
    let x = t.clamp(0.0, 1.0) * HANN_STEPS as f32;
    let i = (x as usize).min(HANN_STEPS - 1);
    let f = x - i as f32;
    table[i] + (table[i + 1] - table[i]) * f
}

pub(super) struct FlowSmearFactory;
impl NodeFactory for FlowSmearFactory {
    fn op_name(&self) -> &'static str {
        "flow-smear"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let field = take_input_ref(fields, "field")?;
        let sides = match read_string_or(fields, "sides", ctx, "both")?.as_str() {
            "both" => Sides::Both,
            "forward" => Sides::Forward,
            other => {
                return Err(FactoryError::BadField {
                    field: "sides".into(),
                    msg: format!("expected `both` or `forward`, got `{other}`"),
                });
            }
        };
        let taper = match read_string_or(fields, "taper", ctx, "flat")?.as_str() {
            "flat" => Taper::Flat,
            "smooth" => Taper::Smooth,
            other => {
                return Err(FactoryError::BadField {
                    field: "taper".into(),
                    msg: format!("expected `flat` or `smooth`, got `{other}`"),
                });
            }
        };

        let mut r = InReader::new(fields, ctx, 2);
        let length = PaddingIn::read_or(&mut r, fields, "length-px", 12.0)?;
        let parts = r.finish();

        let mut ports = vec![
            PortSpec {
                name: "input",
                accepts: ACCEPTS_RASTER_OR_SPRITE,
                optional: false,
            },
            PortSpec {
                name: "field",
                accepts: &[PortKind::Raster],
                optional: false,
            },
        ];
        ports.extend(parts.ports);
        let mut connections = vec![
            Connection {
                port: "input".into(),
                src: input,
            },
            Connection {
                port: "field".into(),
                src: field,
            },
        ];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(FlowSmearNode {
                length,
                sides,
                taper,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Smears `input` along the streamlines of a direction `field` (line integral convolution): each pixel becomes the average of `input` sampled along the curve through it, re-reading the field at every step so strokes bend with it. The field is `flow-field`'s encoding — R/G = 0.5 + 0.5·v in pixel axes, flat grey meaning no direction — and a pixel's walk is `length-px` times the field's length there, so flat ground stays sharp. For hachures or contour strokes, smear a world-anchored `noise` along `dem → flow-field`, then `threshold` or `color-ramp` the result. Seamless across tile borders when `input` is world-anchored; grows the upstream pad by `length-px` + 1.",
            "properties": {
                "input": schema_frag::node_ref(),
                "field": schema_frag::node_ref(),
                "length-px": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "default": 12,
                                "description": "Walk length in px each way, at full field length." })),
                "length-px-max": { "type": "number", "minimum": 0.0, "description": "Upper bound on `length-px` for padding, required when `length-px` is an `@node` port. Values above it are clamped." },
                "sides": { "type": "string", "enum": ["both", "forward"], "default": "both",
                           "description": "`both` smears each way along the field, so a speck becomes a stroke centred on it; `forward` smears only the way the field points, so a speck trails a comet tail downstream." },
                "taper": { "type": "string", "enum": ["flat", "smooth"], "default": "flat",
                           "description": "`flat` weights every sample alike; `smooth` fades them towards the ends of the walk (a Hann window), softening the stroke ends." },
            },
            "required": ["input", "field"],
        })
    }
}

ezu_graph::submit_node!(FlowSmearFactory);
