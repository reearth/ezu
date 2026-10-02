//! Image-processing algorithms, kept apart from the node graph.
//!
//! The nodes under `crate::nodes` read fields, declare ports and padding,
//! and hand pixels to the functions here; this module knows nothing of
//! graphs, ports or tiles. It works on plain RGBA8 buffers.
//!
//! **Every function here produces the same bytes on every host.** A style
//! is meant to render identically natively and in the browser, and the Go
//! package's golden tests pin that down byte for byte. So the arithmetic
//! in this module is restricted to what IEEE 754 and integer arithmetic
//! define exactly:
//!
//! - integer arithmetic, or the basic float operations (`+ - * /`, `sqrt`),
//!   which are correctly rounded everywhere — Rust never contracts them
//!   into fused multiply-adds behind your back;
//! - no `exp`, `sin`, `powf` and the like from the platform: those come
//!   from the system libm natively and from Rust's own on wasm, and the two
//!   can disagree in the last bit. When one is needed, compute it here from
//!   basic operations (see [`exp_neg`]);
//! - no SIMD intrinsics with their own rounding (fused multiply-add,
//!   approximate reciprocals). Plain loops that the compiler vectorises are
//!   fine: vectorising integer or basic float operations does not change
//!   their results.
//!
//! The rule exists because a third-party filter broke it: libblur's NEON
//! path rounds differently from its scalar one, so a blurred tile came out
//! one level apart natively and on wasm.

mod blur;

pub use blur::{gaussian_blur_premultiplied, gaussian_blur_straight};

/// `e^x` for `x <= 0`, from basic float operations only, so every host gets
/// the same bits (see the module docs). Accurate to a few ulp, which is far
/// more than a filter kernel needs; what matters is that it is the same
/// everywhere.
pub(crate) fn exp_neg(x: f64) -> f64 {
    debug_assert!(x <= 0.0, "exp_neg takes non-positive input, got {x}");
    // Below this e^x is subnormal or zero, and no kernel weight that small
    // survives quantisation anyway.
    if x < -708.0 {
        return 0.0;
    }
    // x = k·ln2 + r with |r| <= ln2/2. ln2 is split in two so that k·LN2_HI
    // is exact and the reduction loses nothing. The constants are given as
    // bits so there is no doubt which double each one is.
    const LN2_HI: f64 = f64::from_bits(0x3FE6_2E42_FEE0_0000);
    const LN2_LO: f64 = f64::from_bits(0x3DEA_39EF_3579_3C76);
    const INV_LN2: f64 = f64::from_bits(0x3FF7_1547_652B_82FE);
    let k = (x * INV_LN2).round();
    let r = (x - k * LN2_HI) - k * LN2_LO;
    // e^r by its Taylor series in Horner form; 13 terms reach well below
    // f64 precision for |r| <= 0.35.
    let mut p = 1.0;
    for n in (1..=13).rev() {
        p = 1.0 + p * r / n as f64;
    }
    // 2^k, built from its exponent bits. k >= -1022 here, so it is normal.
    let two_k = f64::from_bits(((1023 + k as i64) as u64) << 52);
    p * two_k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_neg_tracks_the_standard_library() {
        for i in 0..2000 {
            let x = -(i as f64) * 0.0173;
            let want = x.exp();
            let got = exp_neg(x);
            let rel = ((got - want) / want).abs();
            assert!(rel < 1e-14, "exp({x}): {got} vs {want}");
        }
        assert_eq!(exp_neg(0.0), 1.0);
        assert_eq!(exp_neg(-1000.0), 0.0);
    }
}
