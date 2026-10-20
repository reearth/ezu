// Rendering from a SourceSet is the bound table as a value: the same bytes
// at the same places draw the same tile, whichever way they reach the
// render. The same claims go/sources_test.go makes for the Go package.
//
// Run with scripts/test-wasm-js.sh, which builds the module and points
// EZU_WASM_NODE at it.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { test } from "node:test";

const require = createRequire(import.meta.url);
const { Renderer, SourceSet } = require(
  process.env.EZU_WASM_NODE ?? "../../../target/ezu-wasm-node/ezu.js",
);

const repo = new URL("../../../", import.meta.url);
const read = (path) => readFileSync(new URL(path, repo));
const same = (a, b) => Buffer.from(a).equals(Buffer.from(b));

const centreBytes = read("go/testdata/basemap-14-14554-6454.mvt");
const parentBytes = read("go/testdata/basemap-13-7277-3227.mvt");

// Water stitched across tile borders and blurred, so what sits at the
// neighbours reaches the pixels.
const stitched = JSON.stringify({
  name: "stitched",
  "tile-size": 64,
  sources: { basemap: { type: "mvt", url: "https://example.invalid/{z}/{x}/{y}.mvt" } },
  nodes: {
    f: { op: "features", source: "basemap", layer: "water", stitch: "merge" },
    fill: { op: "fill-solid", features: "@f", fill: "#204080" },
    out: { op: "blur", input: "@fill", sigma: 3.0 },
  },
  output: "out",
});

const neighbours = [];
for (let dy = -1; dy <= 1; dy++) {
  for (let dx = -1; dx <= 1; dx++) {
    if (dx !== 0 || dy !== 0) neighbours.push([dx, dy]);
  }
}

test("renderTileWith draws what bindSource draws, neighbours included", () => {
  const r = new Renderer(stitched);
  r.bindSource("basemap", centreBytes);
  for (const coord of neighbours) {
    r.bindSource("basemap", parentBytes, { coord, sourceZoom: 13 });
  }
  const bound = r.renderTile(14, 14554, 6454);
  r.clearSources();

  const centre = r.loadSource("basemap", centreBytes);
  const parent = r.loadSource("basemap", parentBytes);
  assert.equal(centre.source, "basemap");
  const set = new SourceSet();
  assert.ok(set.isEmpty() && !set.contains("basemap"));
  set.bind(centre);
  for (const coord of neighbours) set.bind(parent, { coord, sourceZoom: 13 });
  assert.ok(!set.isEmpty() && set.contains("basemap") && !set.contains("noto"));

  const fromSet = r.renderTileWith(14, 14554, 6454, set);
  assert.ok(same(fromSet, bound), "the render from a set differs from the bound render");
  assert.deepEqual(r.boundSources(), [], "rendering from a set bound nothing");

  const alone = new SourceSet();
  alone.bind(centre);
  assert.ok(
    !same(r.renderTileWith(14, 14554, 6454, alone), fromSet),
    "the neighbours changed nothing, so this compares less than it claims",
  );
  for (const o of [set, alone, centre, parent, r]) o.free();
});

test("one loaded tile serves several renders", () => {
  const r = new Renderer(stitched);
  const parent = r.loadSource("basemap", parentBytes);
  for (const x of [14554, 14555]) {
    const set = new SourceSet();
    set.bind(parent, { sourceZoom: 13 });
    const fromSet = r.renderTileWith(14, x, 6454, set, { format: "rgba" });

    r.bindSource("basemap", parentBytes, { sourceZoom: 13 });
    const bound = r.renderTile(14, x, 6454, { format: "rgba" });
    r.clearSources();
    assert.ok(same(fromSet, bound), `14/${x}/6454 from the shared data differs from binding it`);
    set.free();
  }
  parent.free();
  r.free();
});

test("a set keeps its data, and refuses what bindSource refuses", () => {
  const r = new Renderer(read("go/testdata/labels.json").toString());
  const data = r.loadSource("basemap", centreBytes);
  const set = new SourceSet();
  set.bind(data);
  // Freed, the data lives on in the set that holds it…
  data.free();
  // …and a set's glyphs are the bound table's for the same bytes.
  const fromSet = r.neededCodepointsIn(set);
  const rangesFromSet = r.neededGlyphRangesIn(set);
  r.bindSource("basemap", centreBytes);
  assert.ok(fromSet.noto.length > 0);
  assert.deepEqual(fromSet, r.neededCodepoints());
  assert.deepEqual(rangesFromSet, r.neededGlyphRanges());
  r.clearSources();
  assert.deepEqual(r.neededCodepointsIn(new SourceSet()), { noto: [] });
  assert.ok(r.renderTileWith(14, 14554, 6454, set).length > 0);

  // …but the freed handle itself is gone.
  assert.throws(() => new SourceSet().bind(data));

  const more = r.loadSource("basemap", centreBytes);
  assert.throws(() => set.bind(more, { coord: [2, 0] }), { name: "UnknownSource" });
  assert.throws(() => set.bind(more, { sourceZoom: 99 }), { name: "UnknownSource" });

  assert.throws(() => r.loadSource("noto", new Uint8Array()), { name: "UnknownSource" });
  assert.throws(() => r.loadSource("nowhere", centreBytes), { name: "UnknownSource" });
  assert.throws(() => r.loadSource("basemap", Buffer.from("not a tile")), { name: "MvtDecode" });

  r.trimGlyphs();
  for (const o of [more, set, r]) o.free();
});

test("renderTileWithFeatures hands back a node's features next to the image", () => {
  const r = new Renderer(stitched);
  const set = new SourceSet();
  set.bind(r.loadSource("basemap", centreBytes));
  const plain = r.renderTileWith(14, 14554, 6454, set);

  const { image, features } = r.renderTileWithFeatures(14, 14554, 6454, set, ["f"]);
  assert.ok(same(image, plain), "asking for the features changed the image");
  assert.equal(features.length, 1);
  const [f] = features;
  assert.equal(f.node, "f");
  assert.equal(f.extent, 4096);
  assert.equal(typeof f.geojson, "string");
  const collection = JSON.parse(f.geojson);
  assert.equal(collection.type, "FeatureCollection");
  assert.equal(collection.node, "f");
  assert.equal(collection.extent, 4096);
  assert.ok(collection.features.length > 0, "the tile has water");

  for (const nodes of [["nope"], ["fill"]]) {
    assert.throws(() => r.renderTileWithFeatures(14, 14554, 6454, set, nodes), {
      name: "InvalidStyle",
    });
  }
});

test("the cache budget is the host's and outlives a new style", () => {
  const r = new Renderer(stitched);
  const fallback = r.memoryUsage().cacheBudget;
  r.setCacheBudget(32 << 20);
  r.setStyle(stitched);
  assert.equal(r.memoryUsage().cacheBudget, 32 << 20);
  r.clearCacheBudget();
  assert.equal(r.memoryUsage().cacheBudget, fallback);
});

test("warnings are the current style's graph's", () => {
  // A raster node that names no source falls back to the only one declared,
  // and says so when the graph is built.
  const r = new Renderer(JSON.stringify({
    name: "unnamed",
    sources: { sky: { type: "raster", url: "https://example.invalid/{z}/{x}/{y}.png" } },
    nodes: { out: { op: "raster" } },
    output: "@out",
  }));
  assert.ok(r.warnings.some((w) => w.includes("`source` is not set")), JSON.stringify(r.warnings));
  r.setStyle(stitched);
  assert.deepEqual(r.warnings, []);
  r.free();
});
