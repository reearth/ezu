#!/usr/bin/env bash
# Build the multithreaded (`threads` feature) WASM flavor into
# target/wasm/threads/, alongside the scalar and SIMD builds that
# `wasm-pack` produces.
#
# Requirements:
#   - a nightly toolchain with the `rust-src` component
#     (`rustup toolchain install nightly --component rust-src`)
#   - `wasm-bindgen` and (optionally) `wasm-opt` on PATH
#
# The output is a cross-origin-isolated-only build: the page that loads
# it must be served with `COOP: same-origin` + `COEP: require-corp`
# (see the README). `ezu serve` sets those on `/wasm-demo/`.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT="$ROOT/target/wasm/threads"
TOOLCHAIN="${EZU_WASM_NIGHTLY:-nightly}"
WASM="$ROOT/target/wasm32-unknown-unknown/release/ezu_wasm.wasm"

for tool in cargo wasm-bindgen; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool not found in PATH" >&2
    exit 1
  fi
done

# Atomics + bulk memory are what wasm-bindgen-rayon needs; the memory must
# also be *shared* and imported so it can be posted to worker threads
# (otherwise `initThreadPool` fails with "Memory could not be cloned").
# `-Z build-std` rebuilds std with those target features (nightly only).
echo "→ cargo +$TOOLCHAIN build (wasm32, threads, release)"
RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals \
  -C link-arg=--shared-memory \
  -C link-arg=--import-memory \
  -C link-arg=--max-memory=1073741824 \
  -C link-arg=--export=__heap_base \
  -C link-arg=--export=__wasm_init_tls \
  -C link-arg=--export=__tls_size \
  -C link-arg=--export=__tls_align \
  -C link-arg=--export=__tls_base" \
  cargo "+$TOOLCHAIN" build \
    -p ezu-wasm \
    --release \
    --target wasm32-unknown-unknown \
    --features threads \
    -Z build-std=panic_abort,std

echo "→ wasm-bindgen --target web"
rm -rf "$OUT"
wasm-bindgen "$WASM" \
  --target web \
  --out-dir "$OUT" \
  --out-name ezu_wasm

if command -v wasm-opt >/dev/null 2>&1; then
  echo "→ wasm-opt (threads)"
  wasm-opt -O3 --enable-threads --enable-bulk-memory --enable-mutable-globals \
    "$OUT/ezu_wasm_bg.wasm" \
    -o "$OUT/ezu_wasm_bg.wasm"
else
  echo "note: wasm-opt not found, skipping size optimization"
fi

echo "✓ threads build at $OUT"
