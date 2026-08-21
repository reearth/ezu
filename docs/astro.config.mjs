// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// GitHub Pages for the `reearth/ezu` repository. `base` must match the
// repository name — every internal link is written root-relative and
// Astro rewrites it at build time.
export default defineConfig({
  site: 'https://reearth.github.io',
  base: '/ezu',
  trailingSlash: 'always',
  integrations: [
    starlight({
      title: 'ezu',
      description:
        'Painterly cartography on a pure-Rust, GPU-free CPU renderer with first-class MapLibre compatibility.',
      favicon: '/favicon.svg',
      customCss: ['./src/styles/custom.css'],
      components: {
        // Adds the data credits under the default footer — see the component.
        Footer: './src/components/Footer.astro',
      },
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/reearth/ezu' }],
      editLink: {
        baseUrl: 'https://github.com/reearth/ezu/edit/main/docs/',
      },
      lastUpdated: true,
      tableOfContents: { minHeadingLevel: 2, maxHeadingLevel: 3 },
      sidebar: [
        { label: 'Gallery', link: '/gallery/' },
        {
          label: 'Guides',
          items: [
            { label: 'What is ezu?', link: '/guides/what-is-ezu/' },
            { label: 'Install', link: '/guides/install/' },
            { label: 'Render your first tile', link: '/guides/first-tile/' },
            { label: 'Bounding boxes and pyramids', link: '/guides/bbox-and-pyramids/' },
            { label: 'The live editor', link: '/guides/live-editor/' },
            { label: 'From a MapLibre style', link: '/guides/from-maplibre/' },
            { label: 'Validate in CI', link: '/guides/validate-in-ci/' },
            { label: 'Use from Rust', link: '/guides/rust-library/' },
            { label: 'Use in the browser', link: '/guides/browser-wasm/' },
            { label: 'Serve tiles', link: '/guides/serving-tiles/' },
          ],
        },
        {
          label: 'Concepts',
          items: [
            { label: 'The node graph', link: '/concepts/node-graph/' },
            { label: 'Ports and types', link: '/concepts/ports-and-types/' },
            { label: 'Tiles and determinism', link: '/concepts/tiles-and-determinism/' },
            { label: 'Padding and neighbours', link: '/concepts/padding-and-neighbours/' },
            { label: 'Caching and parallelism', link: '/concepts/caching-and-parallelism/' },
            { label: 'Expressions', link: '/concepts/expressions/' },
            { label: 'Params and functions', link: '/concepts/params-and-functions/' },
            { label: 'Sources and assets', link: '/concepts/sources-and-assets/' },
          ],
        },
        {
          label: 'Style reference',
          items: [
            { label: 'Overview', link: '/style/overview/' },
            { label: 'sources', link: '/style/sources/' },
            { label: 'params', link: '/style/params/' },
            { label: 'functions', link: '/style/functions/' },
            { label: 'nodes and output', link: '/style/nodes-and-output/' },
            { label: 'legend', link: '/style/legend/' },
            { label: 'Expression fields', link: '/style/expression-fields/' },
            { label: 'JSON Schema', link: '/style/json-schema/' },
            { label: 'Node catalog', items: [{ autogenerate: { directory: 'style/nodes' } }] },
          ],
        },
        {
          label: 'MapLibre compatibility',
          items: [
            { label: 'Layer mapping', link: '/maplibre/compatibility/' },
            { label: 'Gaps and differences', link: '/maplibre/gaps-and-differences/' },
            { label: 'Expression conformance', link: '/maplibre/expression-conformance/' },
          ],
        },
        { label: 'Cookbook', items: [{ autogenerate: { directory: 'cookbook' } }] },
        { label: 'Extending', items: [{ autogenerate: { directory: 'extending' } }] },
        { label: 'Performance', items: [{ autogenerate: { directory: 'performance' } }] },
        { label: 'Reference', items: [{ autogenerate: { directory: 'reference' } }] },
      ],
    }),
  ],
});
