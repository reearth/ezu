//! Image-processing algorithms, kept apart from the node graph.
//!
//! The nodes under `crate::nodes` read fields, declare ports and padding,
//! and hand pixels to the functions here; this module knows nothing of
//! graphs, ports or tiles. It works on plain RGBA8 buffers.
//!
//! **Every function here produces the same bytes on every host.** A style
//! is meant to render identically natively and in the browser, and the Go
//! package's golden tests pin that down byte for byte. So the arithmetic
//! in this module is restricted to what comes out the same everywhere:
//!
//! - integer arithmetic, and the float operations IEEE 754 rounds
//!   exactly (`+ - * /`, `sqrt`, `mul_add`) — Rust never contracts a
//!   multiply and an add into a fused one behind your back;
//! - transcendental functions only from the `libm` crate. `f64::exp` and
//!   its kin call the platform's libm natively and Rust's own on wasm, and
//!   the two can disagree in the last bit; `clippy.toml` forbids them
//!   across the workspace for that reason;
//! - no SIMD intrinsics with rounding of their own (approximate
//!   reciprocals and the like). Plain loops the compiler vectorises are
//!   fine: vectorising exact operations cannot change their results.
//!
//! The rule exists because a third-party filter broke it: libblur's NEON
//! path rounds differently from its scalar one, so a blurred tile came out
//! one level apart natively and on wasm.

mod blur;

pub use blur::{gaussian_blur_premultiplied, gaussian_blur_straight};
