// Verifies that every internal link and asset reference in the built site
// resolves. Starlight rewrites base-prefixed URLs at build time, so this runs
// against `dist/` rather than the Markdown sources — which also catches links
// that only exist in generated pages.
//
// `src` counts as much as `href`: the generated node catalog references one
// render per op by path, and dropping a demo from the image generator without
// dropping it from the catalog left a 404 image that an href-only check could
// not see.

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const DIST = resolve(dirname(fileURLToPath(import.meta.url)), '../dist');
const BASE = '/ezu';

function htmlFiles(dir, acc = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) htmlFiles(path, acc);
    else if (entry.name.endsWith('.html')) acc.push(path);
  }
  return acc;
}

if (!existsSync(DIST)) {
  console.error(`no build output at ${DIST} — run \`npm run build\` first`);
  process.exit(1);
}

const broken = new Map();
let checked = 0;

for (const file of htmlFiles(DIST)) {
  const html = readFileSync(file, 'utf8');
  for (const match of html.matchAll(/(?:href|src)="([^"]+)"/g)) {
    const url = match[1];
    if (!url.startsWith(`${BASE}/`)) continue; // external, anchor, or asset-relative
    const path = url.split('#')[0].split('?')[0].slice(BASE.length);
    checked++;
    const target = join(DIST, path);
    if (existsSync(target) || existsSync(join(target, 'index.html'))) continue;
    if (!broken.has(url)) broken.set(url, new Set());
    broken.get(url).add(relative(DIST, file));
  }
}

if (broken.size === 0) {
  console.log(`all ${checked} internal links and assets resolve`);
  process.exit(0);
}

for (const [url, sources] of broken) {
  console.error(`broken: ${url}\n  referenced from: ${[...sources].join(', ')}`);
}
console.error(`\n${broken.size} broken internal reference(s)`);
process.exit(1);
