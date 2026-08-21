// Verifies that every internal link in the built site resolves to a page.
// Starlight rewrites base-prefixed hrefs at build time, so this runs against
// `dist/` rather than the Markdown sources — which also catches links that
// only exist in generated pages.

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
  for (const match of html.matchAll(/href="([^"]+)"/g)) {
    const href = match[1];
    if (!href.startsWith(`${BASE}/`)) continue; // external, anchor, or asset-relative
    const path = href.split('#')[0].split('?')[0].slice(BASE.length);
    checked++;
    const target = join(DIST, path);
    if (existsSync(target) || existsSync(join(target, 'index.html'))) continue;
    if (!broken.has(href)) broken.set(href, new Set());
    broken.get(href).add(relative(DIST, file));
  }
}

if (broken.size === 0) {
  console.log(`all ${checked} internal links resolve`);
  process.exit(0);
}

for (const [href, sources] of broken) {
  console.error(`broken: ${href}\n  linked from: ${[...sources].join(', ')}`);
}
console.error(`\n${broken.size} broken internal link(s)`);
process.exit(1);
