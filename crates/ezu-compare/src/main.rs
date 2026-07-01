//! `ezu-compare` — convert a MapLibre style to an ezu recipe, render it on
//! the CPU with ezu (timed), and pixel-compare against a MapLibre
//! reference (rendered by `tools/mlgl-ref`, or read from `--ref-dir`).
//!
//! Usage:
//! ```text
//! ezu-compare --style <path|url> --tiles 2/2/1,3/4/2 --out <dir> \
//!     [--ref-dir <dir>] [--refgen-dir tools/mlgl-ref] [--threshold 16]
//! ```
//! For each tile it writes `<out>/<z>_<x>_<y>.{ezu,ref,diff}.png` and the
//! converted `<out>/<z>_<x>_<y>.recipe.json`, then prints a summary table.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use ezu::features::mvt;
use ezu::graph::{build_graph, Cache, CanvasInfo, Evaluator, ParamValues, PortValue, TileId};
use ezu::paint::host::{raster_to_rgba8, BrushBankLoader, TileLoader};
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
}

fn parse_args() -> R<Args> {
    let mut style = None;
    let mut tiles = Vec::new();
    let mut out = PathBuf::from("out/compare");
    let mut ref_dir = None;
    let mut refgen_dir = PathBuf::from("tools/mlgl-ref");
    let mut threshold = 16u8;

    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--style" => style = it.next(),
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
        "{:<10} {:>6} {:>8} {:>7} {:>5} {:>9}",
        "tile", "score", "rmse", "diff%", "maxΔ", "ezu(ms)"
    );
    println!("{}", "-".repeat(52));

    let mut rows = Vec::new();
    for &(z, x, y) in &args.tiles {
        match run_tile(&args, &client, &style_json, &style_path, z, x, y) {
            Ok((m, ezu_ms)) => {
                println!(
                    "{:<10} {:>6.2} {:>8.3} {:>6.2}% {:>5} {:>9.1}",
                    format!("{z}/{x}/{y}"),
                    m.score(),
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
        let avg_ms = rows.iter().map(|r| r.4).sum::<f64>() / n;
        println!("{}", "-".repeat(52));
        println!(
            "{:<10} {:>6.2} {:>8} {:>7} {:>5} {:>9.1}",
            "avg", avg_score, "", "", "", avg_ms
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

    // Convert at this tile's zoom so baked zoom-functions are exact here.
    let opts = ezu_maplibre::ConvertOptions {
        zoom: Some(z as f64),
        ..Default::default()
    };
    let (recipe, _report) = ezu_maplibre::convert(style_json, &opts)?;
    let recipe_text = serde_json::to_string_pretty(&recipe)?;
    std::fs::write(args.out.join(format!("{stem}.recipe.json")), &recipe_text)?;

    // ezu render (in-process, timed).
    let (ezu_rgba, size, ezu_ms) = render_ezu(client, &recipe, &recipe_text, z, x, y)?;
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
) -> R<(Vec<u8>, u32, f64)> {
    let doc = Document::from_json(recipe_text)?;
    let tile_size = doc.tile_size;
    let pad = doc.pad;
    let registry = default_registry();
    let graph = build_graph(&doc, &registry)?;
    let mut loader = BrushBankLoader::new();
    loader.register_builtins();
    let cache = Cache::new();
    let tile_id = TileId { z, x, y };

    let mut tile_loader = TileLoader::new(&loader, tile_id);

    // Fetch + bind the vector source (v1 handles MVT-only styles).
    if let Some((src_name, url)) = mvt_source(recipe) {
        let template = resolve_tile_template(client, &url)?;
        let bytes = fetch_tile(client, &template, z, x, y)?;
        let decoded = mvt::decode(&bytes)?;
        tile_loader.bind_mvt(&src_name, decoded);
    }

    let params = ParamValues::new();
    let ev = Evaluator::new(&graph, &cache, &tile_loader);
    let start = Instant::now();
    let out = ev.render(
        tile_id,
        CanvasInfo { tile_size, pad },
        &params,
        tile_seed(z, x, y),
    )?;
    let ezu_ms = start.elapsed().as_secs_f64() * 1000.0;

    let raster = match out {
        PortValue::Raster(r) => r,
        other => return Err(format!("expected Raster output, got {:?}", other.kind()).into()),
    };
    Ok((raster_to_rgba8(&raster, tile_size, pad), tile_size, ezu_ms))
}

/// Find the first MVT source `(name, url)` in a recipe's `sources` block.
fn mvt_source(recipe: &serde_json::Value) -> Option<(String, String)> {
    let sources = recipe.get("sources")?.as_object()?;
    for (name, decl) in sources {
        if decl.get("type").and_then(|v| v.as_str()) == Some("mvt") {
            if let Some(url) = decl.get("url").and_then(|v| v.as_str()) {
                return Some((name.clone(), url.to_string()));
            }
        }
    }
    None
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

fn fetch_tile(
    client: &reqwest::blocking::Client,
    template: &str,
    z: u8,
    x: u32,
    y: u32,
) -> R<Vec<u8>> {
    let url = template
        .replace("{z}", &z.to_string())
        .replace("{x}", &x.to_string())
        .replace("{y}", &y.to_string());
    let resp = client.get(&url).send()?.error_for_status()?;
    Ok(resp.bytes()?.to_vec())
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
