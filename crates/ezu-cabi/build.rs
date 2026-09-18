//! Pin the wasm module to the *reactor* linkage shape.
//!
//! Left alone, `wasm-ld` decides between two shapes by a heuristic, and for a
//! plain `cdylib` it picks *command*-style: it wraps every export in
//!
//! ```text
//! call __wasm_call_ctors → call <the export> → call __wasm_call_dtors
//! ```
//!
//! That runs the atexit destructors after **every** exported call. This crate
//! keeps long-lived state across calls — renderers, their evaluation cache,
//! the brush, font, sprite and glyph banks — and state that outlives a call
//! has no business sitting somewhere teardown runs on each render.
//! Reactor-style, with the constructors run once and nothing torn down
//! afterwards, is what the ABI actually means, so it is asked for rather than
//! hoped for.
//!
//! Exporting `__wasm_call_ctors` is what tells `wasm-ld` to choose reactor. The
//! catch is that the wrappers were also what ran the `inventory::submit!`
//! constructors that register every node op: without them, a host that does not
//! run the constructors itself gets `ezu_op_count() == 0` and every style fails
//! with "unknown op" and nothing naming the cause. Two things prevent that, and
//! both are meant to be there:
//!
//!   - `_initialize` (see `src/lib.rs`), the standard reactor entry point,
//!     which a WASI host calls on instantiation;
//!   - the host calling `__wasm_call_ctors` directly when it is exported, which
//!     is what `go/ezu.go` does. Repeating the constructors is safe: for
//!     `target_family = "wasm"` `inventory` guards each submission with an
//!     `AtomicBool`.
//!
//! This lives in a build script rather than in `.cargo/config.toml` because a
//! `RUSTFLAGS` in the environment replaces that file's `rustflags` wholesale —
//! so building with, say, `RUSTFLAGS="-C target-feature=+simd128"`, which is
//! exactly how the shipped module is built, would silently drop the flag and
//! put the linkage back on the heuristic. A build script travels with the crate
//! and cannot be forgotten or overridden. `rustc-cdylib-link-arg` applies only
//! to the cdylib, leaving the rlib and the native tests alone.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("wasi") {
        println!("cargo:rustc-cdylib-link-arg=--export=__wasm_call_ctors");
    }
}
