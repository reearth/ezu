// Demo recipes for the node catalog's images, one entry per op.
//
// `gen-images.mjs` expands each entry into a complete, standalone style at
// `docs/fixtures/nodes/<op>.json` and renders it — so the image a reader sees
// and the JSON they can copy are the same thing, and `ezu check` can validate
// every one of them.
//
// An entry is `{ nodes, sources?, pad?, note? }`. The generator supplies
// `name`, `tile-size`, the basemap source, and `output: "@out"`, and splices in
// the shared basemap composite whenever the entry references `@base`. Every op
// therefore shows its effect on the same piece of Tokyo, at the same zoom.
//
// Ops absent from this table are listed by the generator as image-less. That is
// deliberate for the ones with nothing to show on their own (`math`, `zoom`,
// `expr`, `switch`, …) — the catalog explains those in prose.

/** The tile every demo renders, and the data behind it. */
export const TILE = { z: 13, x: 7276, y: 3225 };

export const BASEMAP_SOURCE = {
  basemap: { type: 'mvt', url: 'https://papers.reearth.land/protomaps/tilejson.json' },
};

/**
 * A muted flat basemap, spliced in wherever a demo references `@base`. Kept
 * deliberately low-contrast so an op's effect reads on top of it.
 */
export const BASE_NODES = {
  bg: { op: 'solid', color: '#faf6ec' },
  base_earth_f: { op: 'features', layer: 'earth' },
  base_earth: { op: 'fill-solid', features: '@base_earth_f', fill: '#ece1c6' },
  base_water_f: { op: 'features', layer: 'water' },
  base_water: { op: 'fill-solid', features: '@base_water_f', fill: '#8fb4d9' },
  base_roads_f: { op: 'features', layer: 'roads', 'min-zoom-field': 'min_zoom' },
  base_roads: {
    op: 'stroke',
    features: '@base_roads_f',
    color: '#b09a80',
    'width-px': 1.2,
  },
  base: { op: 'stack', layers: ['@bg', '@base_earth', '@base_water', '@base_roads'] },
};

/** Feature-source nodes demos reuse, spliced in on reference. */
export const FEATURE_NODES = {
  water_f: { op: 'features', layer: 'water' },
  earth_f: { op: 'features', layer: 'earth' },
  roads_f: { op: 'features', layer: 'roads', 'min-zoom-field': 'min_zoom' },
  landuse_f: { op: 'features', layer: 'landuse' },
  places_f: { op: 'features', layer: 'places' },
  buildings_f: { op: 'features', layer: 'buildings' },
};

/** Ink used whenever a demo draws geometry over the base. */
const INK = '#c2410c';

/** Draw `features` as a bright stroke over the base — the geometry-op idiom. */
const overBase = (features, width = 1.4) => ({
  draw: { op: 'stroke', features, color: INK, 'width-px': width },
  out: { op: 'blend', base: '@base', over: '@draw' },
});

export const DEMOS = {
  // ── source ────────────────────────────────────────────────────────────────
  features: {
    note: 'Water polygons selected from the vector tile and filled flat.',
    nodes: {
      water_f: { op: 'features', layer: 'water' },
      draw: { op: 'fill-solid', features: '@water_f', fill: INK },
      out: { op: 'blend', base: '@base', over: '@draw' },
    },
  },
  'tile-bounds': {
    crop: false,
    note: 'The tile’s own extent as one polygon, filled translucently.',
    nodes: {
      b: { op: 'tile-bounds' },
      draw: { op: 'fill-solid', features: '@b', fill: INK, 'fill-alpha': 0.35 },
      out: { op: 'blend', base: '@base', over: '@draw' },
    },
  },
  'point-grid': {
    note: 'A synthesized lattice of points, stamped with a small circle each.',
    nodes: {
      g: { op: 'point-grid', spacing: 512, anchor: 'tile' },
      dots: { op: 'circles', features: '@g', color: INK, radius: 3 },
      out: { op: 'blend', base: '@base', over: '@dots' },
    },
  },

  // ── geometry ──────────────────────────────────────────────────────────────
  centroid: {
    note: 'One point per input polygon, drawn as a dot.',
    nodes: {
      c: { op: 'centroid', features: '@water_f' },
      dots: { op: 'circles', features: '@c', color: INK, radius: 5 },
      out: { op: 'blend', base: '@base', over: '@dots' },
    },
  },
  boundary: {
    note: 'Polygon rings turned into lines — the coastline of every water body.',
    nodes: { b: { op: 'boundary', features: '@water_f' }, ...overBase('@b') },
  },
  bbox: {
    crop: false,
    note: 'The bounding box of every input vertex — here, a shrunken cloud of water centroids.',
    nodes: {
      c0: { op: 'centroid', features: '@water_f' },
      c: { op: 'transform', features: '@c0', scale: 0.55 },
      b: { op: 'bbox', features: '@c' },
      bl: { op: 'boundary', features: '@b' },
      dots: { op: 'circles', features: '@c', color: INK, radius: 3 },
      box: { op: 'stroke', features: '@bl', color: INK, 'width-px': 2.5 },
      out: { op: 'stack', layers: ['@base', '@dots', '@box'] },
    },
  },
  'convex-hull': {
    crop: false,
    note: 'The convex hull of every input vertex — here, a shrunken cloud of water centroids.',
    nodes: {
      c0: { op: 'centroid', features: '@water_f' },
      c: { op: 'transform', features: '@c0', scale: 0.55 },
      h: { op: 'convex-hull', features: '@c' },
      hl: { op: 'boundary', features: '@h' },
      dots: { op: 'circles', features: '@c', color: INK, radius: 3 },
      hull: { op: 'stroke', features: '@hl', color: INK, 'width-px': 2.5 },
      out: { op: 'stack', layers: ['@base', '@dots', '@hull'] },
    },
  },
  simplify: {
    note: 'Coastlines simplified with a generous tolerance, so the loss is visible.',
    nodes: {
      b: { op: 'boundary', features: '@water_f' },
      s: { op: 'simplify', features: '@b', epsilon: 24 },
      ...overBase('@s'),
    },
  },
  buffer: {
    note: 'Roads dilated into polygons, then filled.',
    nodes: {
      b: { op: 'buffer', features: '@roads_f', distance: 6 },
      draw: { op: 'fill-solid', features: '@b', fill: INK, 'fill-alpha': 0.5 },
      out: { op: 'blend', base: '@base', over: '@draw' },
    },
  },
  hatch: {
    note: 'Parallel-line hatching of the landmass polygons.',
    nodes: {
      h: { op: 'hatch', features: '@earth_f', spacing: 20, 'angle-deg': 45 },
      ...overBase('@h', 1.0),
    },
  },
  dash: {
    note: 'Coastlines cut into dashes before painting.',
    nodes: {
      b: { op: 'boundary', features: '@water_f' },
      d: { op: 'dash', features: '@b', 'dash-px': 10, 'gap-px': 6 },
      ...overBase('@d', 1.6),
    },
  },
  wave: {
    note: 'Road geometry perturbed sideways — the wobble behind hand-drawn styles.',
    nodes: {
      w: { op: 'wave', features: '@roads_f', 'amplitude-px': 3, 'wavelength-px': 28 },
      ...overBase('@w'),
    },
  },
  smooth: {
    note: 'Coastlines smoothed, rounding off the vertex-to-vertex corners.',
    nodes: {
      b: { op: 'boundary', features: '@water_f' },
      s: { op: 'smooth', features: '@b', iterations: 3 },
      ...overBase('@s'),
    },
  },
  transform: {
    crop: false,
    note: 'Water polygons scaled about their own centre.',
    nodes: {
      t: { op: 'transform', features: '@water_f', scale: 0.55 },
      draw: { op: 'fill-solid', features: '@t', fill: INK, 'fill-alpha': 0.8 },
      out: { op: 'blend', base: '@base', over: '@draw' },
    },
  },
  triangulate: {
    note: 'Building footprints decomposed into triangles, drawn as an outline mesh.',
    nodes: {
      t: { op: 'triangulate', features: '@buildings_f' },
      b: { op: 'boundary', features: '@t' },
      ...overBase('@b', 0.8),
    },
  },
  voronoi: {
    note: 'Voronoi cells around each input point.',
    nodes: {
      c: { op: 'centroid', features: '@buildings_f' },
      v: { op: 'voronoi', features: '@c' },
      b: { op: 'boundary', features: '@v' },
      ...overBase('@b', 1),
    },
  },
  'voronoi-fracture': {
    note: 'Polygons shattered into Voronoi shards — the crackle-glaze effect.',
    nodes: {
      seeds: { op: 'point-grid', spacing: 256, anchor: 'world' },
      v: { op: 'voronoi-fracture', features: '@water_f', seeds: '@seeds' },
      draw: {
        op: 'fill-solid',
        features: '@v',
        fill: '#8fb4d9',
        edge: INK,
        'edge-width': 1.2,
      },
      out: { op: 'blend', base: '@base', over: '@draw' },
    },
  },
  'medial-axis': {
    note: 'The skeleton of each polygon — a river’s centreline from its bank.',
    nodes: {
      s: { op: 'simplify', features: '@water_f', epsilon: 4 },
      m: { op: 'medial-axis', features: '@s', 'densify-px': 8, 'min-branch-px': 12 },
      ...overBase('@m', 2),
    },
  },

  // ── paint ─────────────────────────────────────────────────────────────────
  'fill-solid': {
    note: 'Flat polygon fill, with an outline.',
    nodes: {
      draw: {
        op: 'fill-solid',
        features: '@water_f',
        fill: '#7ea8cf',
        edge: '#2c5f8f',
        'edge-width': 1.5,
      },
      out: { op: 'blend', base: '@base', over: '@draw' },
    },
  },
  'fill-dabs': {
    pad: 32,
    note: 'The same polygons as scatter dabs — many soft, faint stamps instead of coverage.',
    nodes: {
      draw: {
        op: 'fill-dabs',
        features: '@water_f',
        color: '#4a6c99',
        opacity: 0.5,
        'radius-px': 9,
        'spacing-px': 4,
        hardness: 0.5,
        'position-jitter': 0.9,
        'size-jitter': 0.4,
        'opacity-jitter': 0.3,
      },
      out: { op: 'blend', base: '@base', over: '@draw' },
    },
  },
  stroke: {
    note: 'Crisp constant-width stroke, with a dash pattern.',
    nodes: {
      draw: {
        op: 'stroke',
        features: '@roads_f',
        color: INK,
        'width-px': 1.6,
        dasharray: [6, 4],
        cap: 'round',
      },
      out: { op: 'blend', base: '@base', over: '@draw' },
    },
  },
  circles: {
    note: 'A filled circle at every input point, with a stroke.',
    nodes: {
      c: { op: 'centroid', features: '@buildings_f' },
      draw: {
        op: 'circles',
        features: '@c',
        color: '#f4a261',
        radius: 4,
        'stroke-color': '#8a3b12',
        'stroke-width': 1.2,
      },
      out: { op: 'blend', base: '@base', over: '@draw' },
    },
  },

  // ── raster: generators ────────────────────────────────────────────────────
  solid: { note: 'A single flat colour over the whole canvas.', nodes: { out: { op: 'solid', color: '#c2410c' } } },
  circle: {
    note: 'A canvas-sized circle — the sprite behind MapLibre’s `circle` layers.',
    nodes: { out: { op: 'circle', color: INK, 'radius-frac': 0.35, hardness: 0.9 } },
  },
  noise: {
    note: 'Fractal noise, world-anchored so it is continuous across tiles.',
    nodes: {
      out: {
        op: 'noise',
        'scale-px': 96,
        octaves: 4,
        anchor: 'world',
        'low-color': '#faf6ec',
        'high-color': '#8a5a3b',
      },
    },
  },
  'gradient-linear': {
    note: 'A linear gradient across the canvas.',
    nodes: {
      out: {
        op: 'gradient-linear',
        'angle-deg': 35,
        stops: [
          [0, '#faf6ec'],
          [1, '#1f3b57'],
        ],
      },
    },
  },
  'gradient-radial': {
    note: 'A radial gradient from the canvas centre.',
    nodes: {
      out: {
        op: 'gradient-radial',
        stops: [
          [0, '#fde68a'],
          [1, '#7c2d12'],
        ],
      },
    },
  },
  'gradient-conic': {
    note: 'A conic gradient sweeping around the centre.',
    nodes: {
      out: {
        op: 'gradient-conic',
        stops: [
          [0, '#faf6ec'],
          [0.5, '#c2410c'],
          [1, '#faf6ec'],
        ],
      },
    },
  },
  'gradient-diamond': {
    note: 'A diamond gradient — square distance rather than radial.',
    nodes: {
      out: {
        op: 'gradient-diamond',
        stops: [
          [0, '#faf6ec'],
          [1, '#1f3b57'],
        ],
      },
    },
  },

  // ── raster: filters over the base ─────────────────────────────────────────
  blur: { pad: 24, note: 'Gaussian blur, σ = 6 px.', nodes: { out: { op: 'blur', input: '@base', sigma: 6 } } },
  sharpen: { note: 'Unsharp-mask style sharpening.', nodes: { out: { op: 'sharpen', input: '@base', amount: 2.5 } } },
  invert: { note: 'Every channel inverted.', nodes: { out: { op: 'invert', input: '@base' } } },
  hsl: {
    note: 'Hue rotated and saturation raised.',
    nodes: { out: { op: 'hsl', input: '@base', 'hue-shift': 140, saturation: 0.5 } },
  },
  saturate: { note: 'Saturation pushed up.', nodes: { out: { op: 'saturate', input: '@base', amount: 0.8 } } },
  vibrance: {
    note: 'Saturation raised on muted colours only, leaving saturated ones alone.',
    nodes: { out: { op: 'vibrance', input: '@base', amount: 0.9 } },
  },
  'brightness-contrast': {
    note: 'Darkened, with contrast raised around mid-grey.',
    nodes: { out: { op: 'brightness-contrast', input: '@base', brightness: -0.3, contrast: 0.4 } },
  },
  levels: {
    note: 'Input range remapped — the tonal-range expansion before a screen.',
    nodes: { out: { op: 'levels', input: '@base', 'in-black': 0.35, 'in-white': 0.75 } },
  },
  posterize: { note: 'Each channel snapped to four levels.', nodes: { out: { op: 'posterize', input: '@base', steps: 4 } } },
  quantize: {
    note: 'Mapped to a four-ink palette, no dithering.',
    nodes: {
      out: {
        op: 'quantize',
        input: '@base',
        palette: ['#2b2b2b', '#e8dcc0', '#c0703a', '#4a7bb7'],
        space: 'lab',
      },
    },
  },
  dither: {
    note: 'The same four inks, error-diffused — gradients survive the reduction.',
    nodes: {
      out: {
        op: 'dither',
        input: '@base',
        palette: ['#2b2b2b', '#e8dcc0', '#c0703a', '#4a7bb7'],
        method: 'floyd-steinberg',
        space: 'lab',
      },
    },
  },
  mosaic: {
    pad: 24,
    note: 'Averaged into 8 px blocks, on a world-anchored grid.',
    nodes: { out: { op: 'mosaic', input: '@base', block: 8, anchor: 'world' } },
  },
  'edge-detect': {
    note: 'Sobel over a flat water fill — the outline mask the op is usually used for.',
    nodes: {
      mask: { op: 'fill-solid', features: '@water_f', fill: INK },
      e: { op: 'edge-detect', input: '@mask', strength: 3 },
      out: { op: 'blend', base: '@base', over: '@e' },
    },
  },
  'color-to-alpha': {
    note: 'The paper colour knocked out to transparency.',
    nodes: { out: { op: 'color-to-alpha', input: '@base', color: '#faf6ec', tolerance: 0.15 } },
  },
  'channel-shuffle': {
    note: 'Channels permuted — red and blue swapped.',
    nodes: { out: { op: 'channel-shuffle', input: '@base', r: 'b', g: 'g', b: 'r' } },
  },
  warp: {
    pad: 32,
    note: 'Pixels pushed around by internal noise — the paper-wobble op.',
    nodes: { out: { op: 'warp', input: '@base', 'amp-px': 8, 'scale-px': 64 } },
  },
  displace: {
    pad: 32,
    note: 'Displaced by a separate low-frequency noise raster.',
    nodes: {
      d: { op: 'noise', 'scale-px': 64, octaves: 2, anchor: 'world' },
      out: { op: 'displace', input: '@base', displacement: '@d', 'amp-px': 14, boundary: 'mirror' },
    },
  },
  dilate: {
    pad: 24,
    note: 'Dilation of a water mask — coverage grows by the kernel radius.',
    nodes: {
      mask: { op: 'fill-solid', features: '@water_f', fill: INK },
      d: { op: 'dilate', input: '@mask', 'radius-px': 6 },
      out: { op: 'blend', base: '@base', over: '@d' },
    },
  },
  erode: {
    pad: 24,
    note: 'Erosion of the same mask — coverage shrinks, and the narrow channels vanish.',
    nodes: {
      mask: { op: 'fill-solid', features: '@water_f', fill: INK },
      e: { op: 'erode', input: '@mask', 'radius-px': 2 },
      out: { op: 'blend', base: '@base', over: '@e' },
    },
  },
  mix: {
    note: 'A straight crossfade between two rasters.',
    nodes: {
      tint: { op: 'solid', color: '#1f3b57' },
      out: { op: 'mix', a: '@base', b: '@tint', t: 0.45 },
    },
  },
  blend: {
    note: 'A colour multiplied over the base — one of sixteen W3C blend modes.',
    nodes: {
      tint: { op: 'solid', color: '#e9a23b' },
      out: { op: 'blend', base: '@base', over: '@tint', mode: 'multiply', opacity: 0.8 },
    },
  },
  stack: {
    note: 'Several rasters composited bottom-to-top in one pass.',
    nodes: {
      draw_w: { op: 'fill-solid', features: '@water_f', fill: '#7ea8cf' },
      draw_r: { op: 'stroke', features: '@roads_f', color: INK, 'width-px': 1.4 },
      out: { op: 'stack', layers: ['@base', '@draw_w', '@draw_r'] },
    },
  },
};

/** Ops with nothing meaningful to show on their own — documented in prose. */
export const NO_IMAGE = new Set([
  'densify',
  // Emits triangles the downstream paint ops draw nothing for — see
  // https://github.com/reearth/ezu/issues (tracked separately).
  'triangulate',
  'expr',
  'math',
  'zoom',
  'switch',
  'pick-channel',
  'label-placement',
  'text-labels',
  'text-draw',
  'brush-file',
  'brush-solid',
  'image',
  'icon',
  'literal-geometry',
  'resample',
]);
