#!/usr/bin/env bash
# Build the `ezu` npm package: compile the wasm-bindgen crate once, then
# run wasm-bindgen for each JS target (web / bundler / nodejs) on the
# same wasm artifact, dropping outputs into crates/ezu-wasm/npm/.
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

# wasm-bindgen --target nodejs emits CommonJS. The parent package.json
# declares `"type": "module"`, so without this override Node would try to
# load these .js files as ESM and fail.
cat > "$NPM_DIR/nodejs/package.json" <<'EOF'
{ "type": "commonjs" }
EOF

cp "$ROOT/LICENSE-MIT" "$ROOT/LICENSE-APACHE" "$NPM_DIR/"

echo "✓ npm package built at $NPM_DIR"
