#!/usr/bin/env bash
# Build the wasm module the Go package embeds: `crates/ezu-cabi` compiled
# for wasm32-wasip1, size-optimised with binaryen, dropped into `go/`.
#
# `go generate ./...` from `go/` runs this. The result is committed, so
# `go get` needs no Rust toolchain.
#
# The three choices below are measured ones, not defaults:
#
#   * `--release` (opt-level = 3), not a size profile. rustc's
#     `opt-level = "z"` is worth roughly 1.4x in render time — 1.6x on a
#     whole tile — to save about 1.3 MB. A renderer runs for hundreds of
#     milliseconds per tile and this module is server-side, so the trade
#     goes the other way. `codegen-units = 1` is already on in the
#     workspace's release profile, and turning it off costs 600 kB.
#   * `+simd128`. wazero runs it, it is about 17 % faster, and it is
#     smaller than the non-SIMD build at the same opt-level.
#   * `wasm-opt -Oz`, which is binaryen's size pass over already-optimised
#     code and a different lever from rustc's `opt-level = "z"`: about 1 %
#     smaller than `-O3` at no measurable speed cost.
#
# `strip` is deliberately absent. It is a no-op — wasm-opt already drops the
# name section — and it removes the `target_features` custom section, after
# which wasm-opt refuses the module with "memory.copy requires bulk memory"
# unless every feature is re-enabled by hand.
#
# No link flag appears here either. The module's reactor linkage is pinned by
# crates/ezu-cabi/build.rs, which cannot be dropped by the RUSTFLAGS this
# script sets. See that file.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT="$ROOT/go/ezu_cabi.wasm"
BUILT="$ROOT/target/wasm32-wasip1/release/ezu_cabi.wasm"

for tool in cargo wasm-opt; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool not found in PATH" >&2
    exit 1
  fi
done

cd "$ROOT"
RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+simd128" \
  cargo build --release --target wasm32-wasip1 -p ezu-cabi

wasm-opt -Oz --enable-simd "$BUILT" -o "$OUT"

printf 'built %s (%s bytes, from %s bytes)\n' \
  "$OUT" "$(wc -c <"$OUT" | tr -d ' ')" "$(wc -c <"$BUILT" | tr -d ' ')"
