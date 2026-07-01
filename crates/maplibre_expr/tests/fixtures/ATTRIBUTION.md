# Vendored test fixtures

The files under `expression/` are a snapshot of the expression conformance
tests from the [MapLibre style specification][repo], vendored here so the
`maplibre_expr` test suite is hermetic (no network, no submodule).

- **Source**: <https://github.com/maplibre/maplibre-style-spec>
- **Path in source**: `test/integration/expression/tests`
- **Pinned commit**: `ef522e45a28e0efafabbebb27197d3440c99fe34`
- **License**: BSD-3-Clause — Copyright (c) 2020, MapLibre contributors

To refresh, re-run `tests/refresh_fixtures.sh`.

[repo]: https://github.com/maplibre/maplibre-style-spec
