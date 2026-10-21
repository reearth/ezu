# Test fixtures — provenance & licenses

MapLibre GL styles vendored as conversion test inputs. Each is used only to
exercise `ezu-translate`; none is bundled into the library.

## demotiles.json
- Source: <https://demotiles.maplibre.org/style.json> (MapLibre demo style).
- A low-zoom world-countries style: background, fill (with a `match` on
  `ADM0_A3`), lines, and symbol labels.

## versatiles-colorful.json
- Source: <https://tiles.versatiles.org/assets/styles/colorful/style.json>
  (VersaTiles "Colorful"), from
  <https://github.com/versatiles-org/versatiles-style>.
- License: source code is released under **The Unlicense** (public domain);
  sprites/icons under **CC0-1.0**. Data © OpenStreetMap contributors.
- A full OSM basemap style (324 layers) over the **Shortbread** schema, with
  keyless vector tiles at `https://tiles.versatiles.org/tiles/osm/{z}/{x}/{y}`.
- The tests only convert it, offline; nothing they check reads its sprite.
  The vendored copy names its sprite sheet
  `https://tiles.versatiles.org/assets/sprites/basics/sprites`, which
  upstream has since replaced with `…/assets/sprites/base`, so a render of
  this copy fails to load the sheet. To render the style end to end
  (`ezu-compare`, `ezu tile`), translate the live style URL above instead.
