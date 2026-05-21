# ezu-style

**Ezu Style Spec** parser + JSON Schema for the
[`ezu`](../../README.md) workspace.

A minimal `serde`-based reader for the declarative style language ezu
consumes. Unknown fields are rejected (`deny_unknown_fields`) so typos
surface immediately while the spec is in flux.

## Spec at a glance

```json
{
  "name": "watercolor-basic",
  "tile-size": 512,
  "pad": 24,
  "background": "#fbf6e6",
  "layers": [
    { "type": "fill-solid", "id": "earth", "source-layer": "earth", "fill": "#e8d9b0" },

    { "type": "fill-dabs", "id": "water", "source-layer": "water",
      "color": "#5876a0", "opacity": 0.22, "radius-px": 7, "spacing-px": 3 },

    { "type": "line", "id": "roads-motorway", "source-layer": "roads",
      "min-zoom-field": "min_zoom",
      "filter": { "kind_detail": "motorway" },
      "brush": "@watercolor_glazing",
      "color": "#4a3424", "radius-px": 2.6 }
  ]
}
```

Each layer carries an optional `filter` (every entry is AND-combined; values
are single literals or membership lists) and an optional `min-zoom-field`
that points at a numeric MVT property to drop features whose
data-declared minimum zoom is above the rendered zoom.

## Types

```rust
pub struct Style { background: HexColor, layers: Vec<LayerSpec>, tile_size: u32, pad: u32, … }
pub enum LayerSpec { FillSolid(FillSolidSpec), FillDabs(FillDabsSpec), Line(LineSpec) }
pub struct HexColor { r, g, b, a }  // parsed from `#rrggbb` / `#rrggbbaa`, exposes `srgb_linear()`
pub type FeatureFilter = HashMap<String, FilterMatch>;
pub enum FilterMatch { One(FilterAtom), Any(Vec<FilterAtom>) }
pub enum FilterAtom { Bool, Int, Float, Str }
```

## JSON Schema

Every type derives `schemars::JsonSchema`. The checked-in
[`schemas/ezu-style.json`](../../schemas/ezu-style.json) is regenerated
from source with:

```sh
cargo run --bin dump-schema -p ezu-style > schemas/ezu-style.json
```

The same schema is served at `/schemas/ezu-style.json` by
[`ezu-server`](../ezu-server) for client-side validation /
autocomplete.

## Example

```rust
let style = ezu_style::Style::from_json(&json_text)?;
println!("{} ({} layers)", style.name, style.layers.len());
```

See the main [README](../../README.md) for the full project overview.

## License

MIT or Apache-2.0, at your option.
