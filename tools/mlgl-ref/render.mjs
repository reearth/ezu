// Render a single XYZ tile from a MapLibre style using maplibre-gl-js in
// headless Chromium (SwiftShader software WebGL — no real GPU needed).
//
// Usage: node render.mjs <style-url-or-path> <z> <x> <y> <out.png> [size=512]
import { chromium } from "playwright";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const mlglJs = readFileSync(require.resolve("maplibre-gl/dist/maplibre-gl.js"), "utf8");

const [, , styleArg, zArg, xArg, yArg, outArg, sizeArg] = process.argv;
if (!styleArg || zArg === undefined || !outArg) {
  console.error("usage: node render.mjs <style> <z> <x> <y> <out.png> [size]");
  process.exit(2);
}
const z = Number(zArg), x = Number(xArg), y = Number(yArg);
const size = Number(sizeArg ?? 512);

// tile (x+0.5, y+0.5) -> lon/lat of the tile centre (WebMercator / XYZ).
const n = 2 ** z;
const lon = ((x + 0.5) / n) * 360 - 180;
const latRad = Math.atan(Math.sinh(Math.PI * (1 - (2 * (y + 0.5)) / n)));
const lat = (latRad * 180) / Math.PI;

// A style arg may be a URL or a local path; resolve a local path to JSON text
// we can hand maplibre directly, so we don't need a file server.
let style = styleArg;
if (!/^https?:\/\//.test(styleArg)) {
  style = JSON.parse(readFileSync(styleArg, "utf8"));
}

const browser = await chromium.launch({
  args: [
    "--use-gl=angle",
    "--use-angle=swiftshader",
    "--enable-unsafe-swiftshader",
    "--ignore-gpu-blocklist",
  ],
});
const page = await browser.newPage({ viewport: { width: size, height: size }, deviceScaleFactor: 1 });
page.on("console", (m) => { if (m.type() === "error") console.error("[page]", m.text()); });

await page.setContent(
  `<!doctype html><html><head><meta charset="utf-8">
   <style>html,body,#map{margin:0;padding:0;width:${size}px;height:${size}px}</style>
   </head><body><div id="map"></div></body></html>`,
);
await page.addScriptTag({ content: mlglJs });

const dataUrl = await page.evaluate(
  async ({ style, lon, lat, z }) => {
    const map = new maplibregl.Map({
      container: "map",
      style,
      center: [lon, lat],
      zoom: z,
      interactive: false,
      fadeDuration: 0,
      attributionControl: false,
      preserveDrawingBuffer: true, // needed to read pixels back
    });
    await new Promise((res, rej) => {
      map.on("idle", res);
      map.on("error", (e) => rej(new Error(e?.error?.message || "map error")));
      setTimeout(() => rej(new Error("idle timeout")), 30000);
    });
    return map.getCanvas().toDataURL("image/png");
  },
  { style, lon, lat, z },
);

const png = Buffer.from(dataUrl.split(",")[1], "base64");
const { writeFileSync } = await import("node:fs");
writeFileSync(outArg, png);
await browser.close();
console.log(`OK ${z}/${x}/${y} -> ${outArg} (center ${lon.toFixed(5)},${lat.toFixed(5)})`);
