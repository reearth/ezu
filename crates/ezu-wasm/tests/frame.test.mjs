// Print frames from JS: computeFrame, setTileSize and renderFrame do what
// `ezu frame` and the Go package do. The numbers and the hash below came
// from the CLI, and are the ones go/frame_test.go pins.
//
// Run with scripts/test-wasm-js.sh, which builds the module and points
// EZU_WASM_NODE at it.

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { test } from "node:test";

const require = createRequire(import.meta.url);
const { Renderer, SourceSet } = require(
  process.env.EZU_WASM_NODE ?? "../../../target/ezu-wasm-node/ezu.js",
);

const repo = new URL("../../../", import.meta.url);
const read = (path) => readFileSync(new URL(path, repo));
const stainedGlass = () =>
  read("crates/ezu/examples/styles/stained-glass.json").toString();
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

// A 245 × 174 mm page at 1:1000 and 200 dpi over Tokyo, against
// stained-glass's 512 px tile size. The CLI logs, for
//
//   ezu frame --style crates/ezu/examples/styles/stained-glass.json \
//     --center 139.70,35.68 --scale 1000 --dpi 200 --size-px 1929x1370 …
//
// "1929×1370 px at z=19, tile-size 489 (style: 512), scale 1:999.8, shift
// (+0.47, -0.27) px, 20 tiles (465594..465599, 206453..206457)".
test("computeFrame matches the CLI", () => {
  const r = new Renderer(stainedGlass());
  const mm = (length) => Math.round((length / 25.4) * 200);
  const req = {
    centerLon: 139.7,
    centerLat: 35.68,
    scale: 1000,
    dpi: 200,
    width: mm(245),
    height: mm(174),
  };
  assert.deepEqual([req.width, req.height], [1929, 1370]);

  const f = r.computeFrame(req);
  assert.equal(f.zoom, 19);
  assert.equal(f.tileSize, 489);
  assert.ok(Math.abs(f.scale - 999.8) <= 0.05, `scale 1:${f.scale}`);
  assert.ok(Math.abs(f.shift[0] - 0.47) <= 0.005, `shift x ${f.shift[0]}`);
  assert.ok(Math.abs(f.shift[1] + 0.27) <= 0.005, `shift y ${f.shift[1]}`);
  assert.deepEqual(f.tilesX, [465594, 465599]);
  assert.deepEqual(f.tilesY, [206453, 206457]);
  assert.deepEqual([f.width, f.height], [1929, 1370]);
  // The first tile starts at or before the frame's corner, less than a tile
  // before it — which is what places every other tile.
  const dx = f.tilesX[0] * f.tileSize - f.left;
  const dy = f.tilesY[0] * f.tileSize - f.top;
  assert.ok(dx <= 0 && dx > -f.tileSize && dy <= 0 && dy > -f.tileSize, `(${dx}, ${dy})`);
  // Computing a frame changes nothing on the renderer.
  assert.equal(r.tileSize, 512);

  // A fixed zoom derives the tile size from it: one zoom shallower needs
  // tiles twice the size for the same scale.
  const at18 = r.computeFrame({ ...req, zoom: 18 });
  assert.equal(at18.zoom, 18);
  assert.ok(Math.abs(at18.tileSize - 2 * f.tileSize) <= 1, `${at18.tileSize} px`);

  // The style tile size is what the default zoom aims at, so naming a much
  // smaller one moves the zoom deeper: 489 px at z19 is 122 at z21.
  const small = r.computeFrame({ ...req, styleTileSize: 128 });
  assert.deepEqual([small.zoom, small.tileSize], [21, 122]);

  assert.throws(() => r.computeFrame({ ...req, scale: 0 }), { name: "InvalidStyle" });
  assert.throws(() => r.computeFrame({ ...req, dpi: undefined }), { name: "InvalidStyle" });
  assert.throws(() => r.computeFrame({ ...req, zoom: 18.5 }), { name: "InvalidStyle" });
  r.free();
});

test("setTileSize renders at that size", () => {
  const r = new Renderer(stainedGlass());
  const mvt = read("go/testdata/basemap-14-14554-6454.mvt");
  r.bindSource("basemap", mvt);
  const style = r.renderTile(14, 14554, 6454);
  const perRender = r.renderTile(14, 14554, 6454, { format: "rgba", tileSize: 93 });

  r.setTileSize(93);
  assert.equal(r.tileSize, 512, "tileSize reports the style's own");
  const set = r.renderTile(14, 14554, 6454, { format: "rgba" });
  assert.equal(set.length, 93 * 93 * 4);
  assert.ok(Buffer.from(set).equals(Buffer.from(perRender)), "setTileSize(93) differs from a 93 px render");

  // A per-render size still wins.
  assert.equal(r.renderTile(14, 14554, 6454, { format: "rgba", tileSize: 64 }).length, 64 * 64 * 4);

  r.clearTileSize();
  assert.equal(sha256(r.renderTile(14, 14554, 6454)), sha256(style), "after clearTileSize");

  assert.throws(() => r.setTileSize(0), { name: "InvalidStyle" });
  assert.throws(() => r.setTileSize(12.5), { name: "InvalidStyle" });

  // A new style starts at its own size.
  r.setTileSize(93);
  r.setStyle(stainedGlass());
  r.bindSource("basemap", mvt);
  assert.equal(r.renderTile(14, 14554, 6454, { format: "rgba" }).length, 512 * 512 * 4);
  r.free();
});

// A 500 × 400 px frame on the corner where four z14 tiles meet, so every
// tile it renders is cropped on two sides, at a tile size (391) that is
// nobody's default. Only their z13 parent is in go/testdata, so it is
// overzoomed: the CLI falls back to the ancestor when the z14 fetch misses,
// and the bind below says sourceZoom 13. The hash is of the pixels of the
// CLI's PNG, decoded — the same as go/frame_test.go's cornerFramePixels:
//
//   ezu frame --style crates/ezu/examples/styles/stained-glass.json \
//     --mvt '<dir>/{z}/{x}/{y}.mvt' --center 139.812,35.6216 --scale 40000 \
//     --dpi 200 --size-px 500x400 --zoom 14 --out frame.png
const cornerFramePixels = "05a14d1e9de68a304c0186b8dbe49722af5d83e3827dd3aefbad820a39b5e208";

const cornerRequest = {
  centerLon: 139.812,
  centerLat: 35.6216,
  scale: 40000,
  dpi: 200,
  width: 500,
  height: 400,
  zoom: 14,
};

test("renderFrame matches the CLI and the tiles it is made of", async () => {
  const r = new Renderer(stainedGlass());
  const f = r.computeFrame(cornerRequest);
  assert.equal(f.tileSize, 391);
  assert.deepEqual(f.tilesX, [14554, 14556]);
  assert.deepEqual(f.tilesY, [6454, 6456]);

  // All four tiles overzoom the same parent. A host keeps what it loads,
  // keyed by the tile it fetched, and puts it into the set of every frame
  // tile that reads it, so the parent is fetched and decoded once.
  const parent = read("go/testdata/basemap-13-7277-3227.mvt");
  const loaded = new Map();
  const asked = [];
  const sources = async (tile) => {
    asked.push(tile);
    const key = `13/${tile.x >> 1}/${tile.y >> 1}`;
    if (!loaded.has(key)) {
      // A real host fetches; the await is what makes this one asynchronous.
      await new Promise((resolve) => setTimeout(resolve, 0));
      loaded.set(key, r.loadSource("basemap", parent));
    }
    const set = new SourceSet();
    set.bind(loaded.get(key), { sourceZoom: 13 });
    return set;
  };
  const page = await r.renderFrame(f, sources);
  assert.ok(page instanceof Uint8Array);
  assert.equal(page.length, 500 * 400 * 4);
  assert.deepEqual(asked, [
    { z: 14, x: 14554, y: 6454 },
    { z: 14, x: 14555, y: 6454 },
    { z: 14, x: 14554, y: 6455 },
    { z: 14, x: 14555, y: 6455 },
  ]);
  assert.deepEqual([...loaded.keys()], ["13/7277/3227"], "one parent, loaded once");
  assert.deepEqual(r.boundSources(), [], "the frame binds no tile data");
  assert.equal(sha256(page), cornerFramePixels, "the CLI's page");
  for (const data of loaded.values()) data.free();

  // The same page put together by hand from tiles rendered at the frame's
  // tile size, each copied in at its offset and cropped to the page.
  const want = new Uint8Array(f.width * f.height * 4);
  const t = f.tileSize;
  for (let y = f.tilesY[0]; y < f.tilesY[1]; y++) {
    for (let x = f.tilesX[0]; x < f.tilesX[1]; x++) {
      r.clearSources();
      r.bindSource("basemap", parent, { sourceZoom: 13 });
      const tile = r.renderTile(f.zoom, x, y, { format: "rgba", tileSize: t });
      const dx = x * t - f.left;
      const dy = y * t - f.top;
      for (let py = Math.max(dy, 0); py < Math.min(dy + t, f.height); py++) {
        for (let px = Math.max(dx, 0); px < Math.min(dx + t, f.width); px++) {
          const src = ((py - dy) * t + (px - dx)) * 4;
          want.set(tile.subarray(src, src + 4), (py * f.width + px) * 4);
        }
      }
    }
  }
  r.clearSources();
  assert.ok(Buffer.from(page).equals(Buffer.from(want)), "renderFrame differs from its tiles");

  // The renderer's own tile size is untouched.
  r.bindSource("basemap", parent, { sourceZoom: 13 });
  assert.equal(r.renderTile(14, 14554, 6454, { format: "rgba" }).length, 512 * 512 * 4);
  r.clearSources();
  r.free();
});

test("renderFrame refuses what contradicts the frame and passes a sources error on", async () => {
  const r = new Renderer(stainedGlass());
  const f = r.computeFrame(cornerRequest);
  const parent = r.loadSource("basemap", read("go/testdata/basemap-13-7277-3227.mvt"));
  // One set for every tile: the frame reads it and leaves it the host's.
  const set = new SourceSet();
  set.bind(parent, { sourceZoom: 13 });
  const sources = () => set;

  await assert.rejects(r.renderFrame(f, sources, { format: "png" }), { name: "InvalidStyle" });
  await assert.rejects(r.renderFrame(f, sources, { tileSize: 256 }), { name: "InvalidStyle" });
  await assert.rejects(r.renderFrame({ ...f, tileSize: 0 }, sources), { name: "InvalidStyle" });
  await assert.rejects(r.renderFrame({ zoom: 14 }, sources), { name: "InvalidStyle" });

  // A synchronous sources works too, and so does naming the frame's own
  // format and tile size.
  const page = await r.renderFrame(f, sources, { format: "rgba", tileSize: f.tileSize });
  assert.equal(sha256(page), cornerFramePixels);
  assert.ok(set.contains("basemap"), "the set is still the host's");

  // Anything but a set is a mistake, not an empty tile.
  for (const wrong of [undefined, null, {}, parent]) {
    await assert.rejects(r.renderFrame(f, () => wrong), { name: "TypeError" });
  }

  const failure = new Error("fetch failed");
  await assert.rejects(
    r.renderFrame(f, async () => {
      throw failure;
    }),
    (e) => e === failure,
  );
  set.free();
  parent.free();
  r.free();
});

test("a frame holds the renderer until it settles", async () => {
  const r = new Renderer(stainedGlass());
  const f = r.computeFrame(cornerRequest);
  const parent = read("go/testdata/basemap-13-7277-3227.mvt");
  const data = r.loadSource("basemap", parent);
  const setFor = () => {
    const set = new SourceSet();
    set.bind(data, { sourceZoom: 13 });
    return set;
  };
  const busy = { name: "FrameInProgress" };

  let kept;
  const sources = async (tile, binder) => {
    // What another request would do while this tile's data is fetched.
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.throws(() => r.bindSource("basemap", parent), busy);
    assert.throws(() => r.clearSources(), busy);
    assert.throws(() => r.renderTile(14, 14554, 6454), busy);
    assert.throws(() => r.trimGlyphs(), busy);
    assert.throws(() => r.setStyle(stainedGlass()), busy);
    assert.throws(() => r.setTileSize(256), busy);
    assert.throws(() => r.clearTileSize(), busy);
    assert.throws(() => r.setGlyphBudget(1024), busy);
    assert.throws(() => r.setCacheBudget(1 << 20), busy);
    assert.throws(() => r.clearCacheBudget(), busy);
    await assert.rejects(r.renderFrame(f, null), busy);
    // None of these changes what the frame draws.
    r.loadSource("basemap", parent).free();
    assert.equal(r.renderTileWith(14, tile.x, tile.y, setFor(), { format: "rgba" }).length, 512 * 512 * 4);
    // Reading is still fine: a fetch needs these.
    assert.deepEqual(r.sourceTile("basemap", 14, tile.x, tile.y), tile);
    assert.deepEqual(r.boundSources(), []);
    assert.equal(r.tileSize, 512);

    // The previous tile's binder no longer binds.
    if (kept) assert.throws(() => kept.bindSource("basemap", parent), { name: "StaleBinder" });
    kept = binder;
    // Tile data bound would never be read, so the binder refuses it.
    assert.throws(() => binder.bindSource("basemap", parent, { sourceZoom: 13 }), {
      name: "UnknownSource",
    });
    return setFor();
  };
  const page = r.renderFrame(f, sources);
  // Taken at the call, not at the promise's first turn.
  assert.throws(() => r.bindSource("basemap", parent), busy);
  assert.equal(sha256(await page), cornerFramePixels, "the frame came out as it would alone");

  // Settled, the renderer is free again and the last binder is spent.
  assert.throws(() => kept.bindSource("basemap", parent), { name: "StaleBinder" });
  r.bindSource("basemap", parent, { sourceZoom: 13 });
  r.clearSources();
  r.trimGlyphs();

  // A frame that fails gives the renderer back too, with nothing bound.
  await assert.rejects(
    r.renderFrame(f, async () => {
      throw new Error("fetch failed");
    }),
    { message: "fetch failed" },
  );
  assert.deepEqual(r.boundSources(), []);
  data.free();
  await assert.rejects(r.renderFrame({ zoom: 14 }, null), { name: "InvalidStyle" });
  assert.equal(r.renderTile(14, 14554, 6454, { format: "rgba" }).length, 512 * 512 * 4);
  r.free();
});
