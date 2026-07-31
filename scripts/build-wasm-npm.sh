#!/usr/bin/env bash
# Build the `ezu` npm package: compile the wasm-bindgen crate once, then
# run wasm-bindgen for each JS target (web / bundler / nodejs) on the
# same wasm artifact, dropping outputs into crates/ezu-wasm/npm/, then
# generate the workerd entry from the bundler output.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
NPM_DIR="$ROOT/crates/ezu-wasm/npm"
WASM="$ROOT/target/wasm32-unknown-unknown/release/ezu_wasm.wasm"

for tool in cargo wasm-bindgen; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool not found in PATH" >&2
    exit 1
  fi
done

echo "→ cargo build (wasm32-unknown-unknown, release)"
cargo build --release --target wasm32-unknown-unknown -p ezu-wasm

for t in web bundler nodejs; do
  echo "→ wasm-bindgen --target $t"
  rm -rf "$NPM_DIR/$t"
  wasm-bindgen "$WASM" \
    --target "$t" \
    --out-dir "$NPM_DIR/$t" \
    --out-name ezu
done

if command -v wasm-opt >/dev/null 2>&1; then
  for t in web bundler nodejs; do
    echo "→ wasm-opt $t"
    wasm-opt -O3 --enable-simd \
      "$NPM_DIR/$t/ezu_bg.wasm" \
      -o "$NPM_DIR/$t/ezu_bg.wasm"
  done
else
  echo "note: wasm-opt not found, skipping size optimization"
fi

# workerd's `import ... from "*.wasm"` yields an uninstantiated
# `WebAssembly.Module`, not the already-instantiated exports the bundler
# target assumes, so `bundler/ezu.js` fails there with
# "__wbindgen_start is not a function". Emit a workerd entry that
# instantiates explicitly and reuses the bundler glue + wasm.
#
# The import object key must equal the wasm module's import descriptor,
# which wasm-bindgen emits as "./ezu_bg.js" regardless of where this
# entry lives; it is not a module specifier resolved from here.
#
# The re-export list is copied from the bundler entry so the two stay in
# sync. Note that if this crate is ever built with the `threads`
# feature, the `initThreadPool` it adds cannot work on workerd (no
# Worker/SharedArrayBuffer); it would re-export but throw when called.
echo "→ generate workerd entry"
rm -rf "$NPM_DIR/workerd"
mkdir -p "$NPM_DIR/workerd"
exported=$(awk '/^export \{/ { f = 1; next } f && /^\} from/ { exit } f' \
  "$NPM_DIR/bundler/ezu.js" | tr -s ' \n' ' ' | sed 's/^ *//; s/ *$//')
if [ -z "$exported" ]; then
  echo "error: could not read the export list from bundler/ezu.js" >&2
  exit 1
fi
cat > "$NPM_DIR/workerd/ezu.js" <<EOF
/* @ts-self-types="./ezu.d.ts" */
import * as glue from "../bundler/ezu_bg.js";
import wasmModule from "../bundler/ezu_bg.wasm";

// Synchronous instantiation is allowed at module scope on workerd; it
// would be rejected inside a request handler.
const instance = new WebAssembly.Instance(wasmModule, { "./ezu_bg.js": glue });
glue.__wbg_set_wasm(instance.exports);
instance.exports.__wbindgen_start();

export {
    $exported
} from "../bundler/ezu_bg.js";
EOF
cat > "$NPM_DIR/workerd/ezu.d.ts" <<'EOF'
/* tslint:disable */
/* eslint-disable */
export * from "../bundler/ezu.js";
EOF

# wasm-bindgen --target nodejs emits CommonJS. The parent package.json
# declares `"type": "module"`, so without this override Node would try to
# load these .js files as ESM and fail.
cat > "$NPM_DIR/nodejs/package.json" <<'EOF'
{ "type": "commonjs" }
EOF

cp "$ROOT/LICENSE-MIT" "$ROOT/LICENSE-APACHE" "$NPM_DIR/"

echo "✓ npm package built at $NPM_DIR"
