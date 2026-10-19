#!/usr/bin/env bash
# Run the JS tests of the `ezu-wasm` binding under Node. The crate is built
# for wasm32-unknown-unknown and run through wasm-bindgen's nodejs target,
# which is what the npm package's Node entry is, into target/ezu-wasm-node/;
# the tests in crates/ezu-wasm/tests/ load it from there.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT="$ROOT/target/ezu-wasm-node"

for tool in cargo wasm-bindgen node; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool not found in PATH" >&2
    exit 1
  fi
done

cargo build --release --target wasm32-unknown-unknown -p ezu-wasm \
  --manifest-path "$ROOT/Cargo.toml"
rm -rf "$OUT"
wasm-bindgen "$ROOT/target/wasm32-unknown-unknown/release/ezu_wasm.wasm" \
  --target nodejs --out-dir "$OUT" --out-name ezu
# wasm-bindgen's nodejs target is CommonJS.
echo '{ "type": "commonjs" }' > "$OUT/package.json"

EZU_WASM_NODE="$OUT/ezu.js" node --test "$ROOT"/crates/ezu-wasm/tests/*.test.mjs
