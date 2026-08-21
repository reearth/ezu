// Renders the catalog's per-op images from the demo table in node-demos.mjs.
//
// For each op it writes a complete, standalone style to
// `docs/fixtures/nodes/<op>.json` and renders it to
// `docs/public/nodes/<op>.webp`, plus one shared `_base.webp` that filter
// demos use as their "before". Fixtures and images are both committed.
//
// Unlike `gen-reference.mjs`, this is NOT run in CI: it needs a release binary
// and network access to the tile source. Run it by hand when a demo changes:
//
//   npm run images            # everything
//   npm run images -- blur    # only these ops
//
// Rendering happens through the release binary at `target/release/ezu`; build it
// with `cargo build --release -p ezu-cli` first.

import { execFile } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import sharp from 'sharp';

import { BASEMAP_SOURCE, BASE_NODES, DEMOS, FEATURE_NODES, NO_IMAGE, TILE } from './node-demos.mjs';

const run = promisify(execFile);

const DOCS = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const REPO = resolve(DOCS, '..');
const EZU = join(REPO, 'target/release/ezu');
const FIXTURES = join(DOCS, 'fixtures/nodes');
// Published straight out of `public/`: the images are already sized and
// compressed, and a static path keeps the generated MDX free of imports.
const ASSETS = join(DOCS, 'public/nodes');
const TMP = join(REPO, 'target/doc-images');

// Demos render a full 512 px tile, but publish a centre crop at 1:1 rather
// than a downscale: an op like `blur` or `dither` is only legible at native
// pixel scale, and shrinking a busy basemap hides exactly what the image is
// meant to show.
const TILE_SIZE = 512;
const OUT_SIZE = 384;
const CONCURRENCY = 4;

const only = process.argv.slice(2).filter((a) => !a.startsWith('-'));

/** Expand a demo entry into a complete style document. */
function buildStyle(op, demo) {
  const nodes = { ...demo.nodes };
  const text = JSON.stringify(demo.nodes);

  // Splice in the shared pieces the demo refers to, and nothing else, so each
  // fixture stays as small as it can be while remaining standalone.
  for (const [name, node] of Object.entries(FEATURE_NODES)) {
    if (text.includes(`@${name}`)) nodes[name] = node;
  }
  if (text.includes('@base')) Object.assign(nodes, BASE_NODES);

  const needsData = Object.values(nodes).some((n) => n.op === 'features');

  return {
    name: `${op}-demo`,
    'tile-size': TILE_SIZE,
    pad: demo.pad ?? 8,
    ...(needsData ? { sources: BASEMAP_SOURCE } : {}),
    nodes: sortKeys(nodes),
    output: '@out',
  };
}

/** Stable key order, so a regenerated fixture diffs cleanly. */
function sortKeys(nodes) {
  return Object.fromEntries(Object.keys(nodes).sort().map((k) => [k, nodes[k]]));
}

async function render(styleObj, fixturePath, outPath, crop = true) {
  writeFileSync(fixturePath, `${JSON.stringify(styleObj, null, 2)}\n`);
  const png = join(TMP, `${Date.now().toString(36)}-${Math.floor(Math.random() * 1e6)}.png`);
  await run(EZU, ['tile', '--style', fixturePath, '--tile', `${TILE.z}/${TILE.x}/${TILE.y}`, '--out', png], {
    cwd: REPO,
    maxBuffer: 8 * 1024 * 1024,
  });
  // A demo whose subject is the tile itself (`bbox`, `tile-bounds`, a hull)
  // has to be shown whole; everything else reads better cropped at 1:1.
  const inset = Math.floor((TILE_SIZE - OUT_SIZE) / 2);
  const img = sharp(png);
  await (crop
    ? img.extract({ left: inset, top: inset, width: OUT_SIZE, height: OUT_SIZE })
    : img.resize(OUT_SIZE)
  )
    .webp({ quality: 82 })
    .toFile(outPath);
  rmSync(png, { force: true });
}

async function pool(items, worker) {
  const queue = [...items];
  const results = [];
  const runners = Array.from({ length: CONCURRENCY }, async () => {
    for (let item = queue.shift(); item !== undefined; item = queue.shift()) {
      results.push(await worker(item));
    }
  });
  await Promise.all(runners);
  return results;
}

async function main() {
  mkdirSync(FIXTURES, { recursive: true });
  mkdirSync(ASSETS, { recursive: true });
  mkdirSync(TMP, { recursive: true });

  const ops = Object.keys(DEMOS)
    .filter((op) => only.length === 0 || only.includes(op))
    .sort();

  if (only.length === 0) {
    // The shared "before" image for every filter demo.
    const baseStyle = {
      name: 'base-demo',
      'tile-size': TILE_SIZE,
      pad: 8,
      sources: BASEMAP_SOURCE,
      nodes: sortKeys(BASE_NODES),
      output: '@base',
    };
    await render(baseStyle, join(FIXTURES, '_base.json'), join(ASSETS, '_base.webp'));
    console.log('ok   _base');
  }

  const failures = [];
  await pool(ops, async (op) => {
    const style = buildStyle(op, DEMOS[op]);
    try {
      await render(style, join(FIXTURES, `${op}.json`), join(ASSETS, `${op}.webp`), DEMOS[op].crop !== false);
      console.log(`ok   ${op}`);
    } catch (err) {
      const msg = (err.stderr || err.message || '').split('\n').filter(Boolean).pop();
      failures.push([op, msg]);
      console.log(`FAIL ${op}: ${msg}`);
    }
  });

  const missing = Object.keys(DEMOS).length;
  console.log(`\n${ops.length - failures.length}/${ops.length} rendered (${missing} ops in the demo table)`);
  if (NO_IMAGE.size) {
    console.log(`image-less by design: ${[...NO_IMAGE].sort().join(', ')}`);
  }
  if (failures.length) {
    console.log('\nfailures:');
    for (const [op, msg] of failures) console.log(`  ${op}: ${msg}`);
    process.exitCode = 1;
  }
}

main();
