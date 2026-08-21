//! `stack` — composite an ordered list of raster layers bottom-to-top in
//! one pass, with a single accumulator buffer.
//!
//! Equivalent to a chain of `blend(mode: normal, composite: over,
//! opacity: 1)` nodes, but folded into a single n-ary node: the first
//! layer seeds the accumulator and every layer above it is composited in
//! place with the same premultiplied integer source-over used by
//! `blend`'s fast path — so the output is byte-for-byte identical to the
//! chain it replaces, while allocating one buffer instead of N.
//!
//! Only plain source-over is offered here. Anything richer (a per-layer
//! opacity dial, a non-`normal` mode, clipping, a mask, or the eraser
//! composite) stays on `blend`; a translator that meets one splits the
//! stack around it.

use std::sync::Arc;
use std::sync::OnceLock;

use ezu_graph::{
    schema_frag, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx, FactoryError, Node,
    NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    raster_or_sprite_output, unwrap_raster_or_sprite, wrap_raster_like, ACCEPTS_RASTER_OR_SPRITE,
};

use super::blend::div255;

struct StackNode {
    ports: Vec<PortSpec>,
}

impl Node for StackNode {
    fn op_name(&self) -> &'static str {
        "stack"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        // Output mirrors the bottom layer, matching how a `blend` chain
        // propagates the kind of its initial `base`.
        raster_or_sprite_output(input_kinds)
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        // Bottom layer seeds the accumulator; its kind is the output kind.
        let base_in = inputs
            .first()
            .and_then(Option::as_ref)
            .ok_or_else(|| EvalError::MissingInput("layers[0]".into()))?;
        let (base, kind) = unwrap_raster_or_sprite(base_in, "layers[0]")?;

        let mut layers = Vec::with_capacity(inputs.len().saturating_sub(1));
        for (li, slot) in inputs.iter().enumerate().skip(1) {
            let over_in = slot
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput(format!("layers[{li}]")))?;
            layers.push(unwrap_raster_or_sprite(over_in, "layers")?.0);
        }

        match fold_over(&base, &layers)? {
            // At least one layer was composited onto a fresh copy.
            Some(buf) => Ok(wrap_raster_like(Arc::new(buf), kind)),
            // Every layer above the base was blank (or there was only the
            // base): the result is the base buffer verbatim, reused.
            None => Ok(base_in.clone()),
        }
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"stack");
        // Layer count disambiguates otherwise identical input chains.
        h.update(&(self.ports.len() as u64).to_le_bytes());
    }
}

/// Composite `layers` (top-going order) over a copy of `base`, skipping
/// blank layers. Returns `None` when nothing was composited — every layer
/// was blank, or there were none — so the caller can hand back `base`
/// verbatim. Seeding the accumulator lazily on the first non-blank layer,
/// and skipping blanks, reproduce `blend`'s two fast-path shortcuts, so
/// the bytes match a plain-`blend` chain exactly.
fn fold_over(base: &RasterBuf, layers: &[Arc<RasterBuf>]) -> Result<Option<RasterBuf>, EvalError> {
    let (w, h) = (base.width, base.height);
    let mut acc: Option<RasterBuf> = None;
    for over in layers {
        if over.width != w || over.height != h {
            return Err(EvalError::Other("stack: layer size mismatch".into()));
        }
        if over.is_blank() {
            continue;
        }
        let dst = acc.get_or_insert_with(|| base.clone());
        composite_over_inplace(dst, over);
    }
    Ok(acc)
}

/// Plain source-over of a premultiplied RGBA8 `over` onto `acc`, in place
/// and entirely in integer space: `acc_c = over_c + acc_c * (255 - over_a)
/// / 255`. This is exactly `blend`'s `normal_over` fast path at opacity 1
/// (the only opacity `stack` composites at), so a fold of plain `blend`s
/// and a `stack` land on identical bytes.
fn composite_over_inplace(acc: &mut RasterBuf, over: &RasterBuf) {
    for (d, s) in acc
        .pixels
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(over.pixels.as_chunks::<4>().0)
    {
        let inv = 255 - s[3] as u32;
        for c in 0..4 {
            d[c] = (s[c] as u32 + div255(d[c] as u32 * inv)) as u8;
        }
    }
}

/// Interned `&'static str` layer-port names (`layers[0]`, `layers[1]`, …).
/// `PortSpec::name` is `&'static str`; the pool grows once per distinct
/// index ever built and is shared across every `stack` instance, so the
/// leak is bounded by the widest stack seen, not by the number of builds.
fn layer_port_name(ix: usize) -> &'static str {
    static POOL: OnceLock<std::sync::Mutex<Vec<&'static str>>> = OnceLock::new();
    let mut pool = POOL
        .get_or_init(|| std::sync::Mutex::new(Vec::new()))
        .lock()
        .expect("stack port-name pool poisoned");
    while pool.len() <= ix {
        let name: &'static str = Box::leak(format!("layers[{}]", pool.len()).into_boxed_str());
        pool.push(name);
    }
    pool[ix]
}

pub(super) struct StackFactory;
impl NodeFactory for StackFactory {
    fn op_name(&self) -> &'static str {
        "stack"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let arr = fields
            .get("layers")
            .ok_or_else(|| FactoryError::MissingField("layers".into()))?
            .as_array()
            .ok_or_else(|| FactoryError::BadField {
                field: "layers".into(),
                msg: "expected an array of `@node-ref` strings".into(),
            })?;
        if arr.is_empty() {
            return Err(FactoryError::BadField {
                field: "layers".into(),
                msg: "needs at least one layer".into(),
            });
        }

        let mut ports = Vec::with_capacity(arr.len());
        let mut connections = Vec::with_capacity(arr.len());
        for (ix, entry) in arr.iter().enumerate() {
            let s = entry.as_str().ok_or_else(|| FactoryError::BadField {
                field: "layers".into(),
                msg: format!("entry {ix}: expected a `@node-ref` string"),
            })?;
            let id = match ezu_style::FieldRef::classify(s) {
                ezu_style::FieldRef::Node(id) => id.to_string(),
                _ => {
                    return Err(FactoryError::BadField {
                        field: "layers".into(),
                        msg: format!("entry {ix}: expected `@node-ref`, got `{s}`"),
                    })
                }
            };
            let name = layer_port_name(ix);
            ports.push(PortSpec {
                name,
                accepts: ACCEPTS_RASTER_OR_SPRITE,
                optional: false,
            });
            connections.push(Connection {
                port: name.into(),
                src: id,
            });
        }

        Ok(BuiltNode {
            node: Box::new(StackNode { ports }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Composite an ordered list of raster `layers` bottom-to-top with plain source-over (equivalent to a chain of `blend` nodes with `mode: normal`, `composite: over`, `opacity: 1`), folded into one pass over a single accumulator. Fully-transparent layers are skipped. For per-layer opacity, blend modes, clipping, masks, or the eraser composite, use `blend`.",
            "properties": {
                "layers": {
                    "type": "array",
                    "minItems": 1,
                    "items": schema_frag::node_ref(),
                    "description": "Raster (or sprite) layers, bottom first. The output mirrors the bottom layer's kind."
                }
            },
            "required": ["layers"],
        })
    }
}

ezu_graph::submit_node!(StackFactory);

#[cfg(test)]
mod tests {
    use super::*;
    use ezu_graph::{CanvasInfo, NoAssets, ParamValues, TileId};

    /// Tiny deterministic LCG so tests need no `rand` dependency.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
            (self.0 >> 33) as u32
        }
        fn byte(&mut self) -> u8 {
            (self.next() & 0xff) as u8
        }
    }

    fn random_premul(w: u32, h: u32, seed: u64) -> RasterBuf {
        let mut rng = Lcg(seed);
        let mut buf = RasterBuf::new(w, h);
        for px in buf.pixels.as_chunks_mut::<4>().0 {
            let a = rng.byte();
            px[0] = rng.byte().min(a);
            px[1] = rng.byte().min(a);
            px[2] = rng.byte().min(a);
            px[3] = a;
        }
        buf
    }

    /// Reference: replay the layers as an explicit `blend`-chain fold —
    /// seed with the bottom layer, then plain source-over every non-blank
    /// layer above it, one accumulator step at a time.
    fn chain_reference(layers: &[RasterBuf]) -> RasterBuf {
        let mut acc = layers[0].clone();
        for over in &layers[1..] {
            if over.is_blank() {
                continue;
            }
            composite_over_inplace(&mut acc, over);
        }
        acc
    }

    /// Run the node's fold over `layers`, resolving `None` (nothing
    /// composited) back to the bottom layer, so the result is always the
    /// full stacked buffer for comparison.
    fn fold(layers: &[RasterBuf]) -> RasterBuf {
        let arcs: Vec<Arc<RasterBuf>> = layers[1..].iter().cloned().map(Arc::new).collect();
        match fold_over(&layers[0], &arcs).unwrap() {
            Some(buf) => buf,
            None => layers[0].clone(),
        }
    }

    #[test]
    fn two_layers_match_blend_chain() {
        let a = random_premul(31, 17, 0x1111);
        let b = random_premul(31, 17, 0x2222);
        assert_eq!(
            fold(&[a.clone(), b.clone()]).pixels,
            chain_reference(&[a, b]).pixels
        );
    }

    #[test]
    fn many_layers_match_blend_chain() {
        let layers: Vec<RasterBuf> = (0..8)
            .map(|i| random_premul(23, 29, 0xa000 + i as u64))
            .collect();
        assert_eq!(fold(&layers).pixels, chain_reference(&layers).pixels);
    }

    #[test]
    fn blank_layers_are_skipped() {
        let a = random_premul(19, 13, 0x3333);
        let blank = RasterBuf::new(19, 13);
        let b = random_premul(19, 13, 0x4444);
        // Interleaving blanks changes nothing versus compositing a, then b.
        assert_eq!(
            fold(&[a.clone(), blank.clone(), b.clone(), blank]).pixels,
            chain_reference(&[a, b]).pixels
        );
    }

    #[test]
    fn blank_base_promotes_first_opaque_layer() {
        // A blank bottom layer with an opaque layer above must yield that
        // layer's bytes — the `blend` blank-base shortcut.
        let blank = RasterBuf::new(15, 15);
        let b = random_premul(15, 15, 0x5555);
        assert_eq!(fold(&[blank, b.clone()]).pixels, b.pixels);
    }

    #[test]
    fn single_layer_is_returned_verbatim() {
        // No layers above the base: fold composites nothing and reports
        // `None`, so eval hands back the bottom layer untouched.
        let a = random_premul(11, 7, 0x6666);
        let arcs: Vec<Arc<RasterBuf>> = Vec::new();
        assert!(fold_over(&a, &arcs).unwrap().is_none());
    }

    #[test]
    fn size_mismatch_errors() {
        let a = random_premul(8, 8, 0x1);
        let b = random_premul(9, 8, 0x2);
        let arcs = vec![Arc::new(b)];
        assert!(fold_over(&a, &arcs).is_err());
    }

    #[test]
    fn sprite_kind_is_propagated() {
        let ports: Vec<PortSpec> = (0..2)
            .map(|ix| PortSpec {
                name: layer_port_name(ix),
                accepts: ACCEPTS_RASTER_OR_SPRITE,
                optional: false,
            })
            .collect();
        let node = StackNode { ports };
        let a = random_premul(9, 9, 0x7777);
        let b = random_premul(9, 9, 0x8888);
        let inputs = vec![
            Some(PortValue::Sprite(Arc::new(a))),
            Some(PortValue::Sprite(Arc::new(b))),
        ];
        let assets = NoAssets;
        let params = ParamValues::new();
        let ctx = EvalCtx {
            tile: TileId { z: 0, x: 0, y: 0 },
            canvas: CanvasInfo::square(9, 0),
            assets: &assets,
            params: &params,
            rng_seed: 0,
        };
        let out = node.eval(&ctx, &inputs).unwrap();
        assert!(matches!(out, PortValue::Sprite(_)), "kind must mirror base");
    }
}
