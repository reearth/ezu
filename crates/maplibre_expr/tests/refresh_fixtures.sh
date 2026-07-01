#!/usr/bin/env bash
# Refresh the vendored maplibre-style-spec expression fixtures.
#
# Usage: tests/refresh_fixtures.sh [git-ref]
#
# Clones the spec at the given ref (default: main), copies the expression test
# suite into tests/fixtures/expression, and updates the pinned commit recorded
# in tests/fixtures/ATTRIBUTION.md.
set -euo pipefail

REF="${1:-main}"
CRATE_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$CRATE_DIR/tests/fixtures/expression"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

git clone --depth 1 --branch "$REF" \
  https://github.com/maplibre/maplibre-style-spec.git "$TMP/spec" 2>/dev/null \
  || git clone --depth 1 https://github.com/maplibre/maplibre-style-spec.git "$TMP/spec"

COMMIT="$(git -C "$TMP/spec" rev-parse HEAD)"

rm -rf "$DEST"
cp -R "$TMP/spec/test/integration/expression/tests" "$DEST"

echo "Vendored $(find "$DEST" -name test.json | wc -l | tr -d ' ') fixtures at commit $COMMIT"
echo "Remember to update the pinned commit in tests/fixtures/ATTRIBUTION.md: $COMMIT"
