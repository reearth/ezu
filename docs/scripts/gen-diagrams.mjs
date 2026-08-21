// Renders the graph diagrams from real styles.
//
//   npm run diagrams
//
// `ezu graph` already emits Mermaid for any style, so the diagrams come from the
// styles in `docs/fixtures/diagrams/` rather than being drawn by hand — a
// diagram cannot disagree with a graph that ezu actually builds.
//
// One SVG per diagram, shown on a light card in both site themes: `ezu graph`
// emits `classDef` fills chosen for a light background and no text colour, so
// mermaid's dark theme renders pale text on pale fills.
//
// Rendering needs a browser, so this is a manual step like the image
// generators; the SVGs are committed and the site build stays Node-only.
// mermaid-cli is fetched on demand rather than being a dependency, so `npm ci`
// in CI does not pull a Chromium download.
//
// Prerequisites: `cargo build --release -p ezu-cli`, and a Chromium that
// Puppeteer can drive — Playwright's works:
//   npx playwright install chromium

import { execFile } from 'node:child_process';
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const run = promisify(execFile);
const DOCS = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const REPO = resolve(DOCS, '..');
const EZU = join(REPO, 'target/release/ezu');
const FIXTURES = join(DOCS, 'fixtures/diagrams');
const OUT = join(DOCS, 'public/diagrams');
const TMP = join(REPO, 'target/doc-diagrams');

/** Find a Chromium for Puppeteer, preferring one Playwright already installed. */
function chromium() {
  if (process.env.PUPPETEER_EXECUTABLE_PATH) return process.env.PUPPETEER_EXECUTABLE_PATH;
  const root = join(homedir(), 'Library/Caches/ms-playwright');
  if (!existsSync(root)) return undefined;
  const build = readdirSync(root)
    .filter((d) => d.startsWith('chromium-'))
    .sort()
    .pop();
  if (!build) return undefined;
  const path = join(
    root,
    build,
    'chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing'
  );
  return existsSync(path) ? path : undefined;
}

/** Mermaid theme to render with. See the note above about the light card. */
const THEME = 'neutral';

async function main() {
  mkdirSync(OUT, { recursive: true });
  mkdirSync(TMP, { recursive: true });

  const exe = chromium();
  if (!exe) {
    console.error('no Chromium found — run `npx playwright install chromium`');
    process.exitCode = 1;
    return;
  }

  const only = process.argv.slice(2).filter((a) => !a.startsWith('-'));
  const names = readdirSync(FIXTURES)
    .filter((f) => f.endsWith('.json'))
    .map((f) => f.replace(/\.json$/, ''))
    .filter((n) => only.length === 0 || only.includes(n));

  for (const name of names) {
    const style = join(FIXTURES, `${name}.json`);
    const mmd = join(TMP, `${name}.mmd`);
    const { stdout } = await run(EZU, ['graph', style], { cwd: REPO, maxBuffer: 8 * 1024 * 1024 });
    writeFileSync(mmd, stdout);

    const svg = join(OUT, `${name}.svg`);
    await run(
      'npx',
      ['-y', '@mermaid-js/mermaid-cli', '-i', mmd, '-o', svg, '-b', 'transparent', '-t', THEME],
      { cwd: REPO, env: { ...process.env, PUPPETEER_EXECUTABLE_PATH: exe }, maxBuffer: 32 * 1024 * 1024 }
    );
    // mermaid-cli writes `width="100%"` plus a max-width style; keep the
    // max-width as an aspect hint but let the page own the size.
    const text = readFileSync(svg, 'utf8');
    writeFileSync(svg, text.replace(' width="100%"', ''));
    const nodes = JSON.parse(readFileSync(style, 'utf8')).nodes;
    console.log(`ok   ${name} (${Object.keys(nodes).length} nodes)`);
  }

  console.log(`\n${names.length} diagram(s) in public/diagrams`);
}

main();
