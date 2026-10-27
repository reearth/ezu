// Render-time params from JS: a string param is overridden through
// `params` the way a number is, checked against its `enum` there, and
// published in `paramsSchema` for a host to build its controls from.
//
// Run with scripts/test-wasm-js.sh, which builds the module and points
// EZU_WASM_NODE at it.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { test } from "node:test";

const require = createRequire(import.meta.url);
const { Renderer } = require(
  process.env.EZU_WASM_NODE ?? "../../../target/ezu-wasm-node/ezu.js",
);

const style = JSON.stringify({
  name: "modes",
  "tile-size": 16,
  params: {
    mode: { type: "string", enum: ["multiply", "screen"], default: "multiply" },
  },
  nodes: {
    base: { op: "solid", color: "#808080" },
    over: { op: "solid", color: "#ff4020" },
    out: { op: "blend", base: "@base", over: "@over", mode: "$mode" },
  },
  output: "@out",
});

test("a string param is overridden per render and checked against its enum", () => {
  const r = new Renderer(style);
  const mode = r.paramsSchema.properties.mode;
  assert.equal(mode.type, "string");
  assert.deepEqual(mode.enum, ["multiply", "screen"]);
  assert.equal(mode.default, "multiply");

  const render = (params) => r.renderTile(0, 0, 0, { format: "rgba", params });
  const plain = render(undefined);
  assert.deepEqual(render({ mode: "multiply" }), plain);
  assert.notDeepEqual(render({ mode: "screen" }), plain);
  assert.throws(() => render({ mode: "overlay" }), { name: "InvalidStyle" });
});

// A label's fixed-name fields follow a string param per render as well.
const labelStyle = (transform) =>
  JSON.stringify({
    name: "label-case",
    "tile-size": 64,
    params: {
      case: { type: "string", enum: ["none", "uppercase"], default: "none" },
    },
    sources: {
      pins: {
        type: "geojson",
        data: {
          type: "FeatureCollection",
          features: [
            {
              type: "Feature",
              properties: { name: "Ab" },
              geometry: { type: "Point", coordinates: [0, 0] },
            },
          ],
        },
      },
      noto: {
        type: "glyphs",
        url: "https://example.invalid/{fontstack}/{range}.pbf",
        fontstack: "Noto Sans Regular",
      },
    },
    nodes: {
      pins: { op: "features", source: "pins", layer: "pins" },
      fonts: { op: "fonts", "font-stacks": { noto: ["noto"] } },
      out: {
        op: "text",
        features: "@pins",
        fonts: "@fonts",
        font: ["noto"],
        text: ["get", "name"],
        size: 16,
        color: "#000000",
        transform,
      },
    },
    output: "@out",
  });

const glyphs = readFileSync(
  new URL("../../ezu-core/tests/glyphs/0-255.pbf", import.meta.url),
);

test("a string param overrides a label's transform per render", () => {
  const render = (transform, params) => {
    const r = new Renderer(labelStyle(transform));
    r.bindSource("noto", glyphs);
    return r.renderTile(0, 0, 0, { format: "rgba", params });
  };
  const none = render("none");
  const upper = render("uppercase");
  assert.notDeepEqual(upper, none, "the glyphs reached the label");
  assert.deepEqual(render("$case"), none);
  assert.deepEqual(render("$case", { case: "uppercase" }), upper);
});
