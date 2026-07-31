#!/usr/bin/env bash
# Build the `ezu` npm package. The crate is compiled twice — once plain
# and once with `+simd128` — and each wasm artifact is run through
# wasm-bindgen for every JS target (web / bundler / nodejs), dropping
# outputs into crates/ezu-wasm/npm/ (plain) and crates/ezu-wasm/npm/simd/
# (SIMD). A workerd entry is generated from each bundler output.
#
# The package default stays on the plain build: Safari < 16.4 has no
# SIMD128, so a SIMD binary as `.` would break those browsers. Hosts that
# know SIMD is available — Cloudflare Workers, Node, evergreen browsers —
# opt in with `@reearth/ezu/simd`.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
NPM_DIR="$ROOT/crates/ezu-wasm/npm"
# The SIMD pass differs only in RUSTFLAGS, so cargo would reuse the same
# artifact path and the two builds would clobber each other. Give it its
# own target dir.
SIMD_TARGET_DIR="$ROOT/target/wasm-simd"

for tool in cargo wasm-bindgen; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool not found in PATH" >&2
    exit 1
  fi
done

# Emit a workerd entry next to the given build directory.
#
# workerd's `import ... from "*.wasm"` yields an uninstantiated
# `WebAssembly.Module`, not the already-instantiated exports the bundler
# target assumes, so `bundler/ezu.js` fails there with
# "__wbindgen_start is not a function". The generated entry instantiates
# explicitly and reuses the sibling bundler glue + wasm.
#
# The import object key must equal the wasm module's import descriptor,
# which wasm-bindgen emits as "./ezu_bg.js" regardless of where this
# entry lives; it is not a module specifier resolved from here.
#
# The re-export list is copied from the bundler entry so the two stay in
# sync. Note that if this crate is ever built with the `threads`
# feature, the `initThreadPool` it adds cannot work on workerd (no
# Worker/SharedArrayBuffer); it would re-export but throw when called.
#
# $1 — directory holding the bundler/ web/ nodejs/ outputs.
generate_workerd_entry() {
  local dir=$1
  local exported
  exported=$(awk '/^export \{/ { f = 1; next } f && /^\} from/ { exit } f' \
    "$dir/bundler/ezu.js" | tr -s ' \n' ' ' | sed 's/^ *//; s/ *$//')
  if [ -z "$exported" ]; then
    echo "error: could not read the export list from $dir/bundler/ezu.js" >&2
    exit 1
  fi
  rm -rf "$dir/workerd"
  mkdir -p "$dir/workerd"
  cat > "$dir/workerd/ezu.js" <<EOF
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
  cat > "$dir/workerd/ezu.d.ts" <<'EOF'
/* tslint:disable */
/* eslint-disable */
export * from "../bundler/ezu.js";
EOF
}

# Compile, run wasm-bindgen for every JS target, optimize, and generate
# the workerd entry.
#
# $1 — output directory. $2 — label. $3 — extra RUSTFLAGS.
build_variant() {
  local dir=$1 label=$2 rustflags=$3
  local target_dir="$ROOT/target"
  if [ -n "$rustflags" ]; then
    target_dir="$SIMD_TARGET_DIR"
  fi
  local wasm="$target_dir/wasm32-unknown-unknown/release/ezu_wasm.wasm"

  echo "→ cargo build (wasm32-unknown-unknown, release, $label)"
  CARGO_TARGET_DIR="$target_dir" RUSTFLAGS="$rustflags" \
    cargo build --release --target wasm32-unknown-unknown -p ezu-wasm

  for t in web bundler nodejs; do
    echo "→ wasm-bindgen --target $t ($label)"
    rm -rf "${dir:?}/$t"
    wasm-bindgen "$wasm" \
      --target "$t" \
      --out-dir "$dir/$t" \
      --out-name ezu
  done

  if command -v wasm-opt >/dev/null 2>&1; then
    # `--enable-simd` only *permits* v128 opcodes; the vectorization
    # comes from the compiler. Withhold it from the plain build so that
    # binary provably stays SIMD-free for pre-16.4 Safari.
    local opt_flags=(-O3)
    if [ -n "$rustflags" ]; then
      opt_flags+=(--enable-simd)
    fi
    for t in web bundler nodejs; do
      echo "→ wasm-opt $t ($label)"
      wasm-opt "${opt_flags[@]}" \
        "$dir/$t/ezu_bg.wasm" \
        -o "$dir/$t/ezu_bg.wasm"
    done
  else
    echo "note: wasm-opt not found, skipping size optimization"
  fi

  echo "→ generate workerd entry ($label)"
  generate_workerd_entry "$dir"

  # wasm-bindgen --target nodejs emits CommonJS. The parent package.json
  # declares `"type": "module"`, so without this override Node would try
  # to load these .js files as ESM and fail.
  cat > "$dir/nodejs/package.json" <<'EOF'
{ "type": "commonjs" }
EOF
}

build_variant "$NPM_DIR" plain ""
build_variant "$NPM_DIR/simd" simd128 "-C target-feature=+simd128"

cp "$ROOT/LICENSE-MIT" "$ROOT/LICENSE-APACHE" "$NPM_DIR/"

echo "✓ npm package built at $NPM_DIR"
