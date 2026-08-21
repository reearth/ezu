// Figures for the hand-written pages: the guides' step-by-step series, the
// concept pages' side-by-side comparisons, and the gallery.
//
//   npm run figures              # everything
//   npm run figures -- seam-world
//
// Each entry renders through the release binary at `target/release/ezu` and
// lands in `docs/public/figures/<name>.webp`. Inline styles are also written to
// `docs/fixtures/figures/<name>.json` so a reader can reproduce the image.
// Like `npm run images`, this is a manual step — it needs a release build and
// network access — and its output is committed.

import { execFile } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import sharp from 'sharp';

import { BASEMAP_SOURCE, BASE_NODES } from './node-demos.mjs';

const run = promisify(execFile);
const DOCS = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const REPO = resolve(DOCS, '..');
const EZU = join(REPO, 'target/release/ezu');
const OUT = join(DOCS, 'public/figures');
const FIXTURES = join(DOCS, 'fixtures/figures');
const TMP = join(REPO, 'target/doc-images');

const TILE = { z: 13, x: 7276, y: 3225 };
/** Tokyo bay, three tiles across — the gallery's framing. */
const BBOX = '139.72,35.66,139.80,35.72';

const style = (nodes, extra = {}) => ({
  name: 'figure',
  'tile-size': 512,
  pad: 8,
  sources: BASEMAP_SOURCE,
  ...extra,
  nodes,
  output: '@out',
});

/** The `first-tile` guide, one node at a time. */
const STEP_BG = { bg: { op: 'solid', color: '#fbf6e6' } };
const STEP_WATER = { water_f: { op: 'features', layer: 'water' } };

const FIGURES = {
  // ── guides/first-tile ─────────────────────────────────────────────────────
  'step-1-solid': {
    mode: 'tile',
    style: style({ out: { op: 'solid', color: '#fbf6e6' } }),
  },
  'step-2-flat': {
    mode: 'tile',
    style: style({
      ...STEP_BG,
      ...STEP_WATER,
      water: { op: 'fill-solid', features: '@water_f', fill: '#5876a0' },
      out: { op: 'blend', base: '@bg', over: '@water' },
    }),
  },
  'step-3-dabs': {
    mode: 'tile',
    style: style(
      {
        ...STEP_BG,
        ...STEP_WATER,
        water: {
          op: 'fill-dabs',
          features: '@water_f',
          color: '#5876a0',
          opacity: 0.22,
          'radius-px': 7,
          'spacing-px': 3,
          hardness: 0.5,
          'position-jitter': 0.9,
          'size-jitter': 0.4,
          'opacity-jitter': 0.3,
        },
        out: { op: 'blend', base: '@bg', over: '@water' },
      },
      { pad: 32 }
    ),
  },
  'step-4-soft': {
    mode: 'tile',
    style: style(
      {
        ...STEP_BG,
        ...STEP_WATER,
        water: {
          op: 'fill-dabs',
          features: '@water_f',
          color: '#5876a0',
          opacity: 0.22,
          'radius-px': 7,
          'spacing-px': 3,
          hardness: 0.5,
          'position-jitter': 0.9,
          'size-jitter': 0.4,
          'opacity-jitter': 0.3,
        },
        c: { op: 'blend', base: '@bg', over: '@water' },
        out: { op: 'blur', input: '@c', sigma: 3 },
      },
      { pad: 32 }
    ),
  },

  // A pair for `anchor: world` vs `anchor: tile`, and one for a blur with and
  // without margin, both lived here and were removed: at figure scale the
  // difference is not honestly visible, and a side-by-side that shows nothing
  // teaches the wrong thing. The prose on those pages stands on its own.

  // ── cookbook/labels-and-icons ─────────────────────────────────────────────
  'labels-collide': {
    mode: 'tile',
    style: style(
      {
        ...BASE_NODES,
        places_f: { op: 'features', layer: 'places' },
        labels: {
          op: 'text',
          features: '@places_f',
          font: ['sans'],
          text: ['coalesce', ['get', 'name:en'], ['get', 'name']],
          size: 13,
          color: '#3b2f24',
          'halo-color': '#fbf6e6dd',
          'halo-width': 1.6,
          collide: true,
          source: 'basemap',
          layer: 'places',
        },
        out: { op: 'blend', base: '@base', over: '@labels' },
      },
      { pad: 32, sources: { ...BASEMAP_SOURCE, sans: { type: 'font', url: 'system:Helvetica' } } }
    ),
  },
  'labels-no-collide': {
    mode: 'tile',
    style: style(
      {
        ...BASE_NODES,
        places_f: { op: 'features', layer: 'places' },
        labels: {
          op: 'text',
          features: '@places_f',
          font: ['sans'],
          text: ['coalesce', ['get', 'name:en'], ['get', 'name']],
          size: 13,
          color: '#3b2f24',
          'halo-color': '#fbf6e6dd',
          'halo-width': 1.6,
          collide: false,
        },
        out: { op: 'blend', base: '@base', over: '@labels' },
      },
      { pad: 32, sources: { ...BASEMAP_SOURCE, sans: { type: 'font', url: 'system:Helvetica' } } }
    ),
  },

  // ── gallery + landing ─────────────────────────────────────────────────────
  'gallery-watercolor': { mode: 'bbox', file: 'crates/ezu/examples/styles/watercolor.json' },
  'gallery-pencil-sketch': { mode: 'bbox', file: 'crates/ezu/examples/styles/pencil-sketch.json' },
  'gallery-photo-pop': { mode: 'bbox', file: 'crates/ezu/examples/styles/photo-pop.json' },
  'gallery-hillshade': {
    mode: 'bbox',
    file: 'crates/ezu/examples/styles/hillshade.json',
    // Mt Fuji rather than Tokyo bay — a terrain style needs terrain.
    bbox: '138.60,35.30,138.85,35.45',
    zoom: 11,
  },
};

const only = process.argv.slice(2).filter((a) => !a.startsWith('-'));
const tmp = (tag) => join(TMP, `fig-${tag}-${Math.floor(Math.random() * 1e6)}.png`);

async function stylePath(name, fig) {
  if (fig.file) return join(REPO, fig.file);
  const path = join(FIXTURES, `${name}.json`);
  writeFileSync(path, `${JSON.stringify(fig.style, null, 2)}\n`);
  return path;
}

async function renderFigure(name, fig) {
  const path = await stylePath(name, fig);
  const out = join(OUT, `${name}.webp`);
  const width = fig.width ?? 768;

  if (fig.mode === 'bbox') {
    const png = tmp(name);
    await run(
      EZU,
      ['bbox', '--style', path, '--bbox', fig.bbox ?? BBOX, '--zoom', String(fig.zoom ?? 13), '--out', png],
      { cwd: REPO, maxBuffer: 64 * 1024 * 1024 }
    );
    await sharp(png).resize(width).webp({ quality: 84 }).toFile(out);
    rmSync(png, { force: true });
    return;
  }

  if (fig.mode === 'mosaic') {
    // Four adjacent tiles, stitched, so the borders between them are visible.
    const tiles = [
      [0, 0],
      [1, 0],
      [0, 1],
      [1, 1],
    ];
    const parts = [];
    for (const [dx, dy] of tiles) {
      const png = tmp(`${name}-${dx}${dy}`);
      await run(
        EZU,
        ['tile', '--style', path, '--tile', `${TILE.z}/${TILE.x + dx}/${TILE.y + dy}`, '--out', png],
        { cwd: REPO, maxBuffer: 32 * 1024 * 1024 }
      );
      parts.push({ png, dx, dy });
    }
    const buffers = await Promise.all(parts.map((p) => sharp(p.png).png().toBuffer()));
    await sharp({ create: { width: 1024, height: 1024, channels: 4, background: '#00000000' } })
      .composite(buffers.map((input, i) => ({ input, left: parts[i].dx * 512, top: parts[i].dy * 512 })))
      .resize(width)
      .webp({ quality: 84 })
      .toFile(out);
    for (const p of parts) rmSync(p.png, { force: true });
    return;
  }

  const png = tmp(name);
  await run(EZU, ['tile', '--style', path, '--tile', `${TILE.z}/${TILE.x}/${TILE.y}`, '--out', png], {
    cwd: REPO,
    maxBuffer: 32 * 1024 * 1024,
  });
  await sharp(png).resize(width).webp({ quality: 84 }).toFile(out);
  rmSync(png, { force: true });
}

async function main() {
  mkdirSync(OUT, { recursive: true });
  mkdirSync(FIXTURES, { recursive: true });
  mkdirSync(TMP, { recursive: true });

  const names = Object.keys(FIGURES).filter((n) => only.length === 0 || only.includes(n));
  const failures = [];
  for (const name of names) {
    try {
      await renderFigure(name, FIGURES[name]);
      console.log(`ok   ${name}`);
    } catch (err) {
      const msg = (err.stderr || err.message || '').split('\n').filter(Boolean).pop();
      failures.push([name, msg]);
      console.log(`FAIL ${name}: ${msg}`);
    }
  }
  console.log(`\n${names.length - failures.length}/${names.length} figures rendered`);
  if (failures.length) process.exitCode = 1;
}

main();
