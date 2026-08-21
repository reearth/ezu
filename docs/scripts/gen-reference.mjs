// Generates the parts of the site that must not be hand-maintained:
//
//   src/content/docs/style/nodes/*.mdx  — node catalog, one page per category
//   src/content/docs/reference/cli.mdx  — CLI reference
//   public/ezu-style.schema.json        — the served JSON Schema, downloadable
//
// Field shapes and descriptions come from `ezu schema` (every registered
// op's `NodeFactory::schema()`); categories come from the module a node
// lives in under `crates/ezu-paint/src/nodes/<category>/`. Both are
// derived, so a new op shows up here without anyone editing this file.
//
// Output is committed. CI re-runs this and fails on a diff, the same way
// snapshot tests do.

import { execFileSync } from 'node:child_process';
import { readdirSync, readFileSync, writeFileSync, mkdirSync, rmSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const DOCS = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const REPO = resolve(DOCS, '..');
const NODES_DIR = join(REPO, 'crates/ezu-paint/src/nodes');
const OUT_DIR = join(DOCS, 'src/content/docs/style/nodes');

/** Human-facing name and blurb per node module, plus sidebar order. */
const CATEGORIES = {
  source: {
    title: 'Sources',
    order: 1,
    blurb:
      'Ops with no upstream input: features bound by the host, synthesized geometry, images, DEM and raster tiles.',
  },
  paint: {
    title: 'Painting',
    order: 2,
    blurb:
      'Ops that consume `Features` and put pixels on a canvas — flat fills, scatter dabs, brush strokes, stamps, text.',
  },
  brush: {
    title: 'Brushes',
    order: 3,
    blurb: '`() -> Brush` producers: load a MyPaint `.myb` asset or synthesize a brush.',
  },
  geometry: {
    title: 'Geometry',
    order: 4,
    blurb:
      '`Features -> Features` transforms. They rewrite geometry before anything is painted, so downstream paint ops stay simple.',
  },
  raster: {
    title: 'Raster',
    order: 5,
    blurb:
      'Image-processing ops over `Raster` (and, where noted, `Sprite`): generators, composition, colour, warp, morphology, dither, terrain.',
  },
  scalar: {
    title: 'Scalars',
    order: 6,
    blurb:
      'Number producers wired into other nodes’ `In<T>` fields through an `@node` reference.',
  },
  util: { title: 'Utility', order: 7, blurb: 'Plumbing: pick a channel, switch a branch on or off.' },
};

/** op name -> module directory, read from the Rust sources. */
function opCategories() {
  const map = new Map();
  for (const category of readdirSync(NODES_DIR, { withFileTypes: true })) {
    if (!category.isDirectory()) continue;
    const dir = join(NODES_DIR, category.name);
    for (const file of readdirSync(dir)) {
      if (!file.endsWith('.rs') || file === 'mod.rs') continue;
      const src = readFileSync(join(dir, file), 'utf8');
      // Every factory answers `fn op_name(&self) -> &'static str { "…" }`.
      // A file may hold several (e.g. the gradient family).
      for (const m of src.matchAll(/fn op_name\(&self\)\s*->\s*&'static str\s*\{\s*"([^"]+)"/g)) {
        map.set(m[1], category.name);
      }
    }
  }
  return map;
}

function schemaJson() {
  const raw = execFileSync('cargo', ['run', '--quiet', '-p', 'ezu-cli', '--', 'schema'], {
    cwd: REPO,
    maxBuffer: 64 * 1024 * 1024,
    encoding: 'utf8',
  });
  return { text: raw, value: JSON.parse(raw) };
}

function cliHelp() {
  const run = (args) =>
    execFileSync('cargo', ['run', '--quiet', '-p', 'ezu-cli', '--', ...args], {
      cwd: REPO,
      maxBuffer: 8 * 1024 * 1024,
      encoding: 'utf8',
      env: { ...process.env, COLUMNS: '88' },
    });
  const root = run(['--help']);
  const subcommands = [...root.matchAll(/^\s{2}(\w[\w-]*)\s{2,}\S/gm)]
    .map((m) => m[1])
    .filter((name) => name !== 'help');
  return { root, subcommands: subcommands.map((name) => ({ name, help: run([name, '--help']) })) };
}

/** Collapse a JSON Schema fragment into a short human type. */
function typeOf(frag) {
  if (!frag || typeof frag !== 'object') return '—';
  if (frag.const !== undefined) return `\`${JSON.stringify(frag.const)}\``;
  // Reference-shaped string fields read better as the reference syntax
  // than as `string` — the pattern is the tell.
  if (typeof frag.pattern === 'string') {
    if (frag.pattern.startsWith('^@?')) return '`@node`';
    if (frag.pattern.includes('[$@]')) return frag.pattern.includes('#') ? 'color \\| `$param` \\| `@node`' : '`$param` \\| `@node`';
  }
  if (Array.isArray(frag.enum)) return frag.enum.map((v) => `\`${v}\``).join(' \\| ');
  if (Array.isArray(frag.oneOf)) {
    const parts = frag.oneOf.map(typeOf).filter((t) => t !== '—');
    return [...new Set(parts)].join(' \\| ') || '—';
  }
  if (frag.type === 'array') return `array of ${typeOf(frag.items)}`;
  if (typeof frag.type === 'string') {
    let t = frag.type;
    if (frag.minimum !== undefined || frag.maximum !== undefined) {
      const lo = frag.minimum ?? '−∞';
      const hi = frag.maximum ?? '∞';
      t += ` (${lo}…${hi})`;
    }
    return t;
  }
  return '—';
}

/** MDX-safe inline text: escape braces and angle brackets outside code spans. */
function mdx(text) {
  if (!text) return '';
  return text
    .split(/(`[^`]*`)/)
    .map((part) =>
      part.startsWith('`')
        ? part
        : part.replace(/[{}<>]/g, (c) => ({ '{': '&#123;', '}': '&#125;', '<': '&lt;', '>': '&gt;' })[c])
    )
    .join('')
    .replace(/\r?\n\s*/g, ' ')
    .trim();
}

function opSection(op, variant) {
  const props = variant.properties ?? {};
  const required = new Set(variant.required ?? []);
  const rows = Object.entries(props)
    .filter(([name]) => name !== 'op')
    .map(
      ([name, frag]) =>
        `| \`${name}\` | ${typeOf(frag)} | ${required.has(name) ? 'yes' : 'no'} | ${mdx(frag.description) || '—'} |`
    );
  const out = [`### \`${op}\``, ''];
  if (variant.description) out.push(mdx(variant.description), '');
  if (rows.length) {
    out.push('| Field | Type | Required | Description |', '| --- | --- | --- | --- |', ...rows, '');
  } else {
    out.push('Takes no fields beyond `op`.', '');
  }
  return out.join('\n');
}

function main() {
  const categories = opCategories();
  const { text, value } = schemaJson();

  mkdirSync(join(DOCS, 'public'), { recursive: true });
  writeFileSync(join(DOCS, 'public/ezu-style.schema.json'), text);

  const variants = value.$defs?.node?.oneOf ?? value.properties?.nodes?.additionalProperties?.oneOf;
  if (!Array.isArray(variants)) {
    throw new Error('could not find the per-op `oneOf` in the emitted schema — check `ezu schema`');
  }

  const byCategory = new Map();
  const uncategorized = [];
  for (const variant of variants) {
    const op = variant.properties?.op?.const;
    if (!op) continue;
    // `func` is not an op implementation — the registry synthesizes the
    // variant for calls into the document's `functions` block, which the
    // functions page documents.
    if (op === 'func') continue;
    const category = categories.get(op);
    if (!category || !CATEGORIES[category]) {
      uncategorized.push([op, variant]);
      continue;
    }
    if (!byCategory.has(category)) byCategory.set(category, []);
    byCategory.get(category).push([op, variant]);
  }
  if (uncategorized.length) {
    byCategory.set('other', uncategorized);
    CATEGORIES.other = {
      title: 'Other',
      order: 99,
      blurb: 'Ops registered outside the `ezu-paint` node modules.',
    };
  }

  rmSync(OUT_DIR, { recursive: true, force: true });
  mkdirSync(OUT_DIR, { recursive: true });

  let total = 0;
  for (const [category, ops] of byCategory) {
    const meta = CATEGORIES[category];
    ops.sort(([a], [b]) => a.localeCompare(b));
    total += ops.length;
    const body = [
      '---',
      `title: ${JSON.stringify(meta.title)}`,
      `description: ${JSON.stringify(meta.blurb.replace(/`/g, ''))}`,
      'sidebar:',
      `  order: ${meta.order}`,
      '---',
      '',
      '{/* Generated by docs/scripts/gen-reference.mjs — do not edit. */}',
      '',
      meta.blurb,
      '',
      `${ops.length} op${ops.length === 1 ? '' : 's'}: ${ops.map(([op]) => `\`${op}\``).join(', ')}.`,
      '',
      ...ops.map(([op, variant]) => opSection(op, variant)),
    ].join('\n');
    writeFileSync(join(OUT_DIR, `${category}.mdx`), `${body}\n`);
  }

  const { root, subcommands } = cliHelp();
  const cli = [
    '---',
    'title: "CLI reference"',
    'description: "Every ezu command and flag, generated from the CLI itself."',
    'sidebar:',
    '  order: 1',
    '---',
    '',
    '{/* Generated by docs/scripts/gen-reference.mjs — do not edit. */}',
    '',
    'Install with `cargo install ezu-cli`. Every command below also accepts',
    '`--verbose` for per-node evaluator logs.',
    '',
    '## `ezu`',
    '',
    '```text',
    root.trimEnd(),
    '```',
    '',
    ...subcommands.flatMap(({ name, help }) => [
      `## \`ezu ${name}\``,
      '',
      '```text',
      help.trimEnd(),
      '```',
      '',
    ]),
  ].join('\n');
  mkdirSync(join(DOCS, 'src/content/docs/reference'), { recursive: true });
  writeFileSync(join(DOCS, 'src/content/docs/reference/cli.mdx'), cli);

  console.log(
    `wrote ${byCategory.size} catalog pages (${total} ops), CLI reference (${subcommands.length} commands), and the JSON Schema`
  );
}

main();
