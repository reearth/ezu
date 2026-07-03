//! `ezu-compare` — convert a MapLibre style to an ezu recipe, render it on
//! the CPU with ezu (timed), and pixel-compare against a MapLibre
//! reference (rendered by `tools/mlgl-ref`, or read from `--ref-dir`).
//!
//! Usage:
//! ```text
//! ezu-compare --style <path|url> --tiles 2/2/1,3/4/2 --out <dir> \
//!     [--ref-dir <dir>] [--refgen-dir tools/mlgl-ref] [--threshold 16] [--stitch]
//! ```
//! For each tile it writes `<out>/<z>_<x>_<y>.{ezu,ref,diff}.png` and the
//! converted `<out>/<z>_<x>_<y>.recipe.json`, then prints a summary table.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use ezu::features::mvt;
use ezu::graph::{build_graph, Cache, CanvasInfo, Evaluator, ParamValues, PortValue, TileId};
use ezu::paint::host::{
    bind_dem_sources, build_dem_sources, raster_to_rgba8, BrushBankLoader, TileLoader,
};
use ezu::paint::nodes::default_registry;
use ezu::style::Document;
use ezu_compare::{compare_rgba8, diff_image, Metrics};

type R<T> = Result<T, Box<dyn Error>>;

struct Args {
    style: String,
    tiles: Vec<(u8, u32, u32)>,
    out: PathBuf,
    ref_dir: Option<PathBuf>,
    refgen_dir: PathBuf,
    threshold: u8,
    stitch: bool,
}

fn parse_args() -> R<Args> {
    let mut style = None;
    let mut tiles = Vec::new();
    let mut out = PathBuf::from("out/compare");
    let mut ref_dir = None;
    let mut refgen_dir = PathBuf::from("tools/mlgl-ref");
    let mut threshold = 16u8;
    let mut stitch = false;

    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--style" => style = it.next(),
            "--stitch" => stitch = true,
            "--tiles" => {
                let s = it.next().ok_or("--tiles needs a value")?;
                for t in s.split(',') {
                    let p: Vec<&str> = t.split('/').collect();
                    if p.len() != 3 {
                        return Err(format!("bad tile `{t}` (want z/x/y)").into());
                    }
                    tiles.push((p[0].parse()?, p[1].parse()?, p[2].parse()?));
                }
            }
            "--out" => out = it.next().ok_or("--out needs a value")?.into(),
            "--ref-dir" => ref_dir = it.next().map(PathBuf::from),
            "--refgen-dir" => refgen_dir = it.next().ok_or("--refgen-dir needs a value")?.into(),
            "--threshold" => threshold = it.next().ok_or("--threshold needs a value")?.parse()?,
            other => return Err(format!("unknown arg `{other}`").into()),
        }
    }
    Ok(Args {
        style: style.ok_or("--style is required")?,
        tiles: if tiles.is_empty() {
            return Err("--tiles is required (e.g. 2/2/1)".into());
        } else {
            tiles
        },
        out,
        ref_dir,
        refgen_dir,
        threshold,
        stitch,
    })
}

fn main() -> R<()> {
    let args = parse_args()?;
    std::fs::create_dir_all(&args.out)?;

    // The reference renderer wants the original style; load its text once
    // (from disk or HTTP) both for conversion and to pass a stable path to
    // the reference generator.
    let style_text = read_style(&args.style)?;
    let style_json: serde_json::Value = serde_json::from_str(&style_text)?;
    // Stage the style locally so the Node reference renderer reads the exact
    // same bytes we convert (avoids a second network fetch / drift).
    let style_path = args.out.join("_style.json");
    std::fs::write(&style_path, &style_text)?;

    let client = reqwest::blocking::Client::builder()
        .user_agent("ezu-compare")
        .build()?;

    println!(
        "{:<10} {:>6} {:>6} {:>8} {:>7} {:>5} {:>9}",
        "tile", "score", "ssim", "rmse", "diff%", "maxΔ", "ezu(ms)"
    );
    println!("{}", "-".repeat(60));

    let mut rows = Vec::new();
    for &(z, x, y) in &args.tiles {
        match run_tile(&args, &client, &style_json, &style_path, z, x, y) {
            Ok((m, ezu_ms)) => {
                println!(
                    "{:<10} {:>6.2} {:>6.3} {:>8.3} {:>6.2}% {:>5} {:>9.1}",
                    format!("{z}/{x}/{y}"),
                    m.score(),
                    m.ssim,
                    m.rmse,
                    m.diff_fraction * 100.0,
                    m.max_diff,
                    ezu_ms,
                );
                rows.push((z, x, y, m, ezu_ms));
            }
            Err(e) => println!("{:<10} ERROR: {e}", format!("{z}/{x}/{y}")),
        }
    }

    if !rows.is_empty() {
        let n = rows.len() as f64;
        let avg_score = rows.iter().map(|r| r.3.score()).sum::<f64>() / n;
        let avg_ssim = rows.iter().map(|r| r.3.ssim).sum::<f64>() / n;
        let avg_ms = rows.iter().map(|r| r.4).sum::<f64>() / n;
        println!("{}", "-".repeat(60));
        println!(
            "{:<10} {:>6.2} {:>6.3} {:>8} {:>7} {:>5} {:>9.1}",
            "avg", avg_score, avg_ssim, "", "", "", avg_ms
        );
        println!(
            "\nOutputs (ezu / ref / diff PNGs + recipe) in {}",
            args.out.display()
        );
    }
    Ok(())
}

/// Convert → render ezu (timed) → get reference → compare → write outputs.
fn run_tile(
    args: &Args,
    client: &reqwest::blocking::Client,
    style_json: &serde_json::Value,
    style_path: &Path,
    z: u8,
    x: u32,
    y: u32,
) -> R<(Metrics, f64)> {
    let stem = format!("{z}_{x}_{y}");

    // Recipes are zoom-independent: zoom/data functions are emitted as raw
    // `*-expr` and evaluated per tile, so one conversion serves every zoom.
    let opts = ezu_translate::maplibre::ConvertOptions::default();
    let (recipe, _report) = ezu_translate::maplibre::convert(style_json, &opts)?;
    let recipe_text = serde_json::to_string_pretty(&recipe)?;
    std::fs::write(args.out.join(format!("{stem}.recipe.json")), &recipe_text)?;

    // ezu render (in-process, timed).
    let (ezu_rgba, size, ezu_ms) = render_ezu(client, &recipe, &recipe_text, z, x, y, args.stitch)?;
    save_rgba(&args.out.join(format!("{stem}.ezu.png")), &ezu_rgba, size)?;

    // Reference RGBA (from precomputed dir, or freshly generated by Node).
    let ref_rgba = obtain_reference(args, style_path, z, x, y, size)?;
    save_rgba(&args.out.join(format!("{stem}.ref.png")), &ref_rgba, size)?;

    let m = compare_rgba8(&ezu_rgba, &ref_rgba, size, size, args.threshold)
        .ok_or("size mismatch between ezu and reference")?;
    let diff = diff_image(&ezu_rgba, &ref_rgba, size, size, 2.0);
    save_rgba(&args.out.join(format!("{stem}.diff.png")), &diff, size)?;

    Ok((m, ezu_ms))
}

/// Render one tile from an ezu recipe in-process. Returns (RGBA8, size,
/// render-milliseconds). Timing covers only the graph evaluation, not the
/// MVT fetch/decode, so it reflects ezu's CPU rendering cost.
fn render_ezu(
    client: &reqwest::blocking::Client,
    recipe: &serde_json::Value,
    recipe_text: &str,
    z: u8,
    x: u32,
    y: u32,
    stitch: bool,
) -> R<(Vec<u8>, u32, f64)> {
    let doc = Document::from_json(recipe_text)?;
    let tile_size = doc.tile_size;
    let pad = doc.pad;
    let registry = default_registry();
    let graph = build_graph(&doc, &registry)?;
    let mut loader = BrushBankLoader::new();
    loader.register_builtins();
    // Sprite sources: fetch the atlas PNG + index JSON and register the sheet
    // so `icon` nodes can crop named icons for symbol / fill-pattern layers.
    for (name, decl) in &doc.sources {
        if let ezu::style::SourceDecl::Sprite(sprite) = decl {
            match load_sprite_sheet(client, sprite) {
                Ok(sheet) => loader.insert_sprite(sprite.image.clone(), sheet),
                Err(e) => eprintln!("sprite source `{name}`: {e}"),
            }
        }
    }
    let cache = Cache::new();
    let tile_id = TileId { z, x, y };

    let mut tile_loader = TileLoader::new(&loader, tile_id);

    // Fetch + bind the vector source. With `--stitch`, merge the 3×3 tile
    // neighbourhood into the centre tile's frame so geometry near the edges
    // fills ezu's pad ring — matching how maplibre-gl-js renders a viewport
    // that spans tile borders (mirrors the host's DEM/raster stitch). This
    // only affects pad-sampling ops (blur / warp / dab) at tile edges, since
    // the output is cropped to the tile; it multiplies decode/render cost, so
    // it's opt-in. Plain fill/line output is unchanged.
    for (src_name, url) in mvt_sources(recipe) {
        let template = resolve_tile_template(client, &url)?;
        let decoded = if stitch {
            stitch_mvt(client, &template, z, x, y)?
        } else {
            fetch_decoded(client, &template, z, x, y)?
        };
        if let Some(decoded) = decoded {
            tile_loader.bind_mvt(&src_name, decoded);
        }
    }

    // Fetch/parse + bind GeoJSON sources. The data is WGS84 lon/lat, so it's
    // projected into this tile's local frame (extent 4096) and bound as a
    // single feature layer under `<source>.<source>` — matching the recipe's
    // `features` node, which targets `(source, source)` for geojson layers.
    for (src_name, data) in geojson_sources(client, recipe)? {
        match ezu::features::geojson::decode_projected(&data, z, x, y, 4096) {
            Ok(features) => {
                let layer = ezu::features::FeatureLayer {
                    name: src_name.clone(),
                    extent: 4096,
                    features,
                };
                tile_loader.bind_features(format!("{src_name}.{src_name}"), layer);
            }
            Err(e) => eprintln!("geojson source `{src_name}`: {e}"),
        }
    }

    // Fetch + bind DEM sources (for hillshade/terrain). The binder is async
    // (stitches the 3×3 neighbourhood over HTTP); run it on a scratch
    // runtime — this is data prep, outside the timed render below.
    let canvas = CanvasInfo { tile_size, pad };
    let dem_sources = build_dem_sources(&doc);
    if !dem_sources.is_empty() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(bind_dem_sources(
            &mut tile_loader,
            &dem_sources,
            tile_id,
            canvas,
        ))?;
    }

    let params = ParamValues::new();
    let ev = Evaluator::new(&graph, &cache, &tile_loader);
    let start = Instant::now();
    let out = ev.render(tile_id, canvas, &params, tile_seed(z, x, y))?;
    let ezu_ms = start.elapsed().as_secs_f64() * 1000.0;

    let raster = match out {
        PortValue::Raster(r) => r,
        other => return Err(format!("expected Raster output, got {:?}", other.kind()).into()),
    };
    Ok((raster_to_rgba8(&raster, tile_size, pad), tile_size, ezu_ms))
}

/// Find the first MVT source `(name, url)` in a recipe's `sources` block.
/// All MVT sources in a recipe as `(name, url)`, so every vector source is
/// fetched and bound (ezu namespaces layers as `<source>.<layer>`).
fn mvt_sources(recipe: &serde_json::Value) -> Vec<(String, String)> {
    recipe
        .get("sources")
        .and_then(|s| s.as_object())
        .into_iter()
        .flatten()
        .filter(|(_, decl)| decl.get("type").and_then(|v| v.as_str()) == Some("mvt"))
        .filter_map(|(name, decl)| {
            decl.get("url")
                .and_then(|v| v.as_str())
                .map(|url| (name.clone(), url.to_string()))
        })
        .collect()
}

/// Fetch a sprite source's atlas PNG + index JSON (HTTP) and build the sheet.
fn load_sprite_sheet(
    client: &reqwest::blocking::Client,
    sprite: &ezu::style::SpriteSource,
) -> R<ezu::graph::SpriteSheet> {
    let atlas_bytes = client
        .get(&sprite.image)
        .send()?
        .error_for_status()?
        .bytes()?;
    let atlas = ezu::paint::host::decode_image_bytes(&atlas_bytes)
        .map_err(|e| format!("atlas decode: {e}"))?;
    let fetched = match &sprite.index {
        ezu::style::SpriteIndex::Url(u) => Some(client.get(u).send()?.error_for_status()?.text()?),
        ezu::style::SpriteIndex::Inline(_) => None,
    };
    let icons = ezu::paint::host::build_sprite_icons(&sprite.index, fetched.as_deref())?;
    Ok(ezu::graph::SpriteSheet { atlas, icons })
}

/// All GeoJSON sources in a recipe as `(name, data)`, resolving each to its
/// GeoJSON document: inline `data` objects are used directly; a `url` (or a
/// string `data`) is fetched over HTTP.
fn geojson_sources(
    client: &reqwest::blocking::Client,
    recipe: &serde_json::Value,
) -> R<Vec<(String, serde_json::Value)>> {
    let mut out = Vec::new();
    let Some(srcs) = recipe.get("sources").and_then(|s| s.as_object()) else {
        return Ok(out);
    };
    for (name, decl) in srcs {
        if decl.get("type").and_then(|v| v.as_str()) != Some("geojson") {
            continue;
        }
        // Inline object/array `data` is the document itself; a string `data`
        // or a `url` points at a remote document to fetch.
        let data = match decl.get("data") {
            Some(d) if d.is_object() || d.is_array() => d.clone(),
            other => {
                let url = other
                    .and_then(|v| v.as_str())
                    .or_else(|| decl.get("url").and_then(|v| v.as_str()));
                let Some(url) = url else {
                    eprintln!("geojson source `{name}`: no `data`/`url` — skipped");
                    continue;
                };
                let body = client.get(url).send()?.error_for_status()?.text()?;
                serde_json::from_str(&body)?
            }
        };
        out.push((name.clone(), data));
    }
    Ok(out)
}

/// Turn a source url into an `{z}/{x}/{y}` template. If it's a TileJSON
/// document, follow it to its first `tiles` entry; otherwise assume the url
/// is already a template.
fn resolve_tile_template(client: &reqwest::blocking::Client, url: &str) -> R<String> {
    if url.contains("{z}") {
        return Ok(url.to_string());
    }
    let body = client.get(url).send()?.error_for_status()?.text()?;
    let json: serde_json::Value = serde_json::from_str(&body)?;
    let tmpl = json
        .get("tiles")
        .and_then(|t| t.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .ok_or("TileJSON has no tiles[] template")?;
    Ok(tmpl.to_string())
}

/// Fetch + decode one MVT tile. `None` on a 404 (missing tile), so callers
/// can tolerate absent neighbours.
fn fetch_decoded(
    client: &reqwest::blocking::Client,
    template: &str,
    z: u8,
    x: u32,
    y: u32,
) -> R<Option<mvt::DecodedTile>> {
    let url = template
        .replace("{z}", &z.to_string())
        .replace("{x}", &x.to_string())
        .replace("{y}", &y.to_string());
    let resp = client.get(&url).send()?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let bytes = resp.error_for_status()?.bytes()?;
    Ok(Some(mvt::decode(&bytes)?))
}

/// Fetch the centre tile and its 8 neighbours, translating each neighbour's
/// geometry into the centre tile's coordinate frame (offset by `±extent`).
/// Returns the merged tile, or `None` if even the centre tile is absent.
fn stitch_mvt(
    client: &reqwest::blocking::Client,
    template: &str,
    z: u8,
    x: u32,
    y: u32,
) -> R<Option<mvt::DecodedTile>> {
    let Some(mut center) = fetch_decoded(client, template, z, x, y)? else {
        return Ok(None);
    };
    let n = 1u32 << z; // tiles per axis at this zoom
    for dy in -1i32..=1 {
        for dx in -1i32..=1 {
            if dx == 0 && dy == 0 {
                continue;
            }
            // x wraps around the antimeridian; y has no wrap.
            let ny = y as i32 + dy;
            if ny < 0 || ny >= n as i32 {
                continue;
            }
            let nx = (x as i32 + dx).rem_euclid(n as i32) as u32;
            if let Some(neighbour) = fetch_decoded(client, template, z, nx, ny as u32)? {
                merge_neighbour(&mut center, neighbour, dx, dy);
            }
        }
    }
    Ok(Some(center))
}

/// Append a neighbour tile's features into `center`, offsetting geometry by
/// `(dx, dy)` tiles (in that layer's `extent` units).
fn merge_neighbour(center: &mut mvt::DecodedTile, neighbour: mvt::DecodedTile, dx: i32, dy: i32) {
    for mut layer in neighbour.layers {
        let ox = dx * layer.extent as i32;
        let oy = dy * layer.extent as i32;
        for f in &mut layer.features {
            offset_geometry(&mut f.geometry, ox, oy);
        }
        match center.layers.iter_mut().find(|l| l.name == layer.name) {
            Some(existing) => existing.features.append(&mut layer.features),
            None => center.layers.push(layer),
        }
    }
}

fn offset_geometry(g: &mut ezu::features::Geometry, ox: i32, oy: i32) {
    let shift = |p: &mut (i32, i32)| {
        p.0 += ox;
        p.1 += oy;
    };
    g.points.iter_mut().for_each(shift);
    for line in &mut g.lines {
        line.iter_mut().for_each(shift);
    }
    for poly in &mut g.polygons {
        poly.exterior.iter_mut().for_each(shift);
        for hole in &mut poly.holes {
            hole.iter_mut().for_each(shift);
        }
    }
}

/// Get the reference tile as RGBA8 of `size × size`. Prefers a precomputed
/// `--ref-dir/<z>_<x>_<y>.png`; otherwise runs the Node reference renderer.
fn obtain_reference(
    args: &Args,
    style_path: &Path,
    z: u8,
    x: u32,
    y: u32,
    size: u32,
) -> R<Vec<u8>> {
    let stem = format!("{z}_{x}_{y}");
    let path = if let Some(dir) = &args.ref_dir {
        dir.join(format!("{stem}.png"))
    } else {
        let out = args.out.join(format!("{stem}.ref-src.png"));
        run_refgen(&args.refgen_dir, style_path, z, x, y, size, &out)?;
        out
    };
    load_rgba(&path, size)
}

fn run_refgen(
    refgen_dir: &Path,
    style_path: &Path,
    z: u8,
    x: u32,
    y: u32,
    size: u32,
    out: &Path,
) -> R<()> {
    let style_abs = std::fs::canonicalize(style_path)?;
    let out_abs = std::path::absolute(out)?;
    let status = Command::new("node")
        .current_dir(refgen_dir)
        .arg("render.mjs")
        .arg(&style_abs)
        .arg(z.to_string())
        .arg(x.to_string())
        .arg(y.to_string())
        .arg(&out_abs)
        .arg(size.to_string())
        // Keep the renderer's per-tile "OK …" chatter out of the table;
        // stderr (real errors) still surfaces.
        .stdout(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("failed to run node reference renderer: {e} (is Node installed + `npm install` run in {}?)", refgen_dir.display()))?;
    if !status.success() {
        return Err(format!("reference renderer exited with {status}").into());
    }
    Ok(())
}

fn read_style(style: &str) -> R<String> {
    if style.starts_with("http://") || style.starts_with("https://") {
        Ok(reqwest::blocking::get(style)?.error_for_status()?.text()?)
    } else {
        Ok(std::fs::read_to_string(style)?)
    }
}

fn load_rgba(path: &Path, size: u32) -> R<Vec<u8>> {
    let img = image::open(path)?.to_rgba8();
    if img.width() != size || img.height() != size {
        return Err(format!(
            "{}: expected {size}x{size}, got {}x{}",
            path.display(),
            img.width(),
            img.height()
        )
        .into());
    }
    Ok(img.into_raw())
}

fn save_rgba(path: &Path, rgba: &[u8], size: u32) -> R<()> {
    let img = image::RgbaImage::from_raw(size, size, rgba.to_vec())
        .ok_or("rgba buffer wrong size for image")?;
    img.save(path)?;
    Ok(())
}

/// Same deterministic per-tile seed the ezu CLI uses, so brush jitter etc.
/// match a CLI render of the same recipe.
fn tile_seed(z: u8, x: u32, y: u32) -> u64 {
    let mut s = 0u64;
    for v in [z as u64, x as u64, y as u64] {
        s = s.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(v);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::{merge_neighbour, offset_geometry};
    use ezu::features::mvt::DecodedTile;
    use ezu::features::{Feature, FeatureLayer, Geometry, Polygon};
    use std::collections::HashMap;

    fn feat(points: &[(i32, i32)]) -> Feature {
        Feature {
            id: None,
            geometry: Geometry {
                points: points.to_vec(),
                lines: vec![],
                polygons: vec![],
            },
            properties: HashMap::new(),
        }
    }

    fn tile(name: &str, extent: u32, features: Vec<Feature>) -> DecodedTile {
        DecodedTile {
            layers: vec![FeatureLayer {
                name: name.into(),
                extent,
                features,
            }],
        }
    }

    #[test]
    fn offset_shifts_all_geometry() {
        let mut g = Geometry {
            points: vec![(1, 2)],
            lines: vec![vec![(3, 4), (5, 6)]],
            polygons: vec![Polygon {
                exterior: vec![(7, 8)],
                holes: vec![vec![(9, 10)]],
            }],
        };
        offset_geometry(&mut g, 100, 200);
        assert_eq!(g.points, [(101, 202)]);
        assert_eq!(g.lines[0], [(103, 204), (105, 206)]);
        assert_eq!(g.polygons[0].exterior, [(107, 208)]);
        assert_eq!(g.polygons[0].holes[0], [(109, 210)]);
    }

    #[test]
    fn merge_east_neighbour_offsets_by_extent_and_appends() {
        let mut center = tile("water", 4096, vec![feat(&[(10, 10)])]);
        let east = tile("water", 4096, vec![feat(&[(5, 5)])]);
        merge_neighbour(&mut center, east, 1, 0); // dx=1 → +extent in x
        let feats = &center.layers[0].features;
        assert_eq!(feats.len(), 2);
        assert_eq!(feats[1].geometry.points, [(4101, 5)]);
    }

    #[test]
    fn merge_creates_missing_layer() {
        let mut center = tile("water", 4096, vec![]);
        let north = tile("roads", 4096, vec![feat(&[(0, 0)])]);
        merge_neighbour(&mut center, north, 0, -1); // dy=-1 → -extent in y
        assert!(center.layers.iter().any(|l| l.name == "roads"));
        let roads = center.layers.iter().find(|l| l.name == "roads").unwrap();
        assert_eq!(roads.features[0].geometry.points, [(0, -4096)]);
    }
}
