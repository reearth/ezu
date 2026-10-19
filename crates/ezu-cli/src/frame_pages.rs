//! `ezu frame --pages`: the pages of one job, read from a JSON file.
//!
//! A print job is often dozens of pages from one style. Rendered one process
//! per page, each page reads the style again, builds the graph again and
//! fetches the background tiles it shares with its neighbours again. One run
//! over a pages file does that work once. Each page here stands for what one
//! `ezu frame` run's flags say, and renders to the same bytes.
//!
//! ```json
//! { "defaults": { "dpi": 200, "size-mm": [245, 174] },
//!   "pages": [
//!     { "center": [139.70, 35.66], "scale": 1000, "out": "p1.png",
//!       "features": { "boxes": { "out": "p1.geojson", "key": ["label-id", "part"] } } },
//!     { "center": [139.71, 35.66], "scale": 2500, "out": "p2.png" } ] }
//! ```

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::frame_features::FeatureCoords;
use crate::OutputFormat;

/// One page, every field settled: what the single-page flags give.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Page {
    pub center: (f64, f64),
    pub scale: f64,
    pub dpi: f64,
    pub width: u32,
    pub height: u32,
    pub zoom: Option<u8>,
    pub out: PathBuf,
    pub format: OutputFormat,
    pub features: Vec<(String, PathBuf)>,
    pub features_key: Vec<(String, Vec<String>)>,
    pub features_coords: FeatureCoords,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PagesFile {
    #[serde(default)]
    defaults: Option<serde_json::Value>,
    pages: Vec<serde_json::Value>,
}

/// A page's fields as written, or the defaults for every page. Each field
/// has the name of the flag it stands for.
#[derive(Deserialize, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Fields {
    center: Option<[f64; 2]>,
    scale: Option<f64>,
    dpi: Option<f64>,
    size_mm: Option<[f64; 2]>,
    size_px: Option<[u32; 2]>,
    zoom: Option<u8>,
    out: Option<PathBuf>,
    format: Option<OutputFormat>,
    features: Option<BTreeMap<String, FeaturesOut>>,
    features_coords: Option<FeatureCoords>,
}

/// One entry of `features`: `--features NODE=out` and, with `key`,
/// `--features-key NODE=key`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FeaturesOut {
    out: PathBuf,
    #[serde(default)]
    key: Option<Vec<String>>,
}

impl Fields {
    fn parse(value: serde_json::Value, label: &str) -> Result<Self, String> {
        let fields: Fields = serde_json::from_value(value).map_err(|e| format!("{label}: {e}"))?;
        if fields.size_mm.is_some() && fields.size_px.is_some() {
            return Err(format!(
                "{label}: `size-mm` and `size-px` are mutually exclusive"
            ));
        }
        Ok(fields)
    }
}

/// Read the pages file at `path`. Relative paths in it are taken from the
/// file's own directory, as a style's are from the style's. Every page is
/// checked before any is rendered, so a mistake on the last page costs
/// nothing.
pub(crate) fn read(path: &Path) -> Result<Vec<Page>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("pages file {}: {e}", path.display()))?;
    let base = path.parent().unwrap_or(Path::new(""));
    parse(&text, base).map_err(|e| format!("pages file {}: {e}", path.display()))
}

fn parse(text: &str, base: &Path) -> Result<Vec<Page>, String> {
    let file: PagesFile = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if file.pages.is_empty() {
        return Err("`pages` is empty".into());
    }
    let defaults = match file.defaults {
        Some(v) => Fields::parse(v, "defaults")?,
        None => Fields::default(),
    };
    if defaults.out.is_some() {
        return Err("defaults: `out` is given per page".into());
    }
    let mut written: HashMap<PathBuf, usize> = HashMap::new();
    let mut pages = Vec::with_capacity(file.pages.len());
    for (i, value) in file.pages.into_iter().enumerate() {
        let n = i + 1;
        let label = format!("page {n}");
        let page = settle(Fields::parse(value, &label)?, &defaults, base)
            .map_err(|e| format!("{label}: {e}"))?;
        let outputs = std::iter::once(&page.out).chain(page.features.iter().map(|(_, p)| p));
        for out in outputs {
            if let Some(first) = written.insert(out.clone(), n) {
                return Err(format!(
                    "{label}: {} is already written by page {first}",
                    out.display()
                ));
            }
        }
        pages.push(page);
    }
    Ok(pages)
}

/// One page's fields over the defaults. `features` and the paper size each
/// replace the defaults' whole: a page that gives `size-px` does not also
/// inherit a default `size-mm`.
fn settle(page: Fields, defaults: &Fields, base: &Path) -> Result<Page, String> {
    fn required<T: Clone>(field: &str, own: Option<T>, default: &Option<T>) -> Result<T, String> {
        own.or_else(|| default.clone())
            .ok_or_else(|| format!("`{field}` is required, on the page or in `defaults`"))
    }
    let center = required("center", page.center, &defaults.center)?;
    let scale = required("scale", page.scale, &defaults.scale)?;
    let dpi = required("dpi", page.dpi, &defaults.dpi)?;
    if !(dpi.is_finite() && dpi > 0.0) {
        return Err(format!("`dpi` must be a positive number, got {dpi}"));
    }
    let own_size = page.size_mm.is_some() || page.size_px.is_some();
    let (size_mm, size_px) = if own_size {
        (page.size_mm, page.size_px)
    } else {
        (defaults.size_mm, defaults.size_px)
    };
    let (width, height) = match (size_px, size_mm) {
        (Some([w, h]), _) if w > 0 && h > 0 => (w, h),
        (Some([w, h]), _) => return Err(format!("`size-px` [{w}, {h}] has a zero side")),
        (None, Some([w, h])) if [w, h].iter().all(|v| v.is_finite() && *v > 0.0) => {
            (crate::mm_to_px(w, dpi), crate::mm_to_px(h, dpi))
        }
        (None, Some([w, h])) => {
            return Err(format!(
                "`size-mm` [{w}, {h}] is not two positive numbers of millimetres"
            ))
        }
        (None, None) => {
            return Err(
                "one of `size-mm` or `size-px` is required, on the page or in `defaults`".into(),
            )
        }
    };
    let out = base.join(page.out.ok_or("`out` is required")?);
    let format = page
        .format
        .or(defaults.format)
        .unwrap_or_else(|| OutputFormat::from_path(&out));

    let mut features = Vec::new();
    let mut features_key = Vec::new();
    let entries = page.features.as_ref().or(defaults.features.as_ref());
    for (node, entry) in entries.into_iter().flatten() {
        features.push((node.clone(), base.join(&entry.out)));
        if let Some(key) = &entry.key {
            if key.is_empty() || key.iter().any(|p| p.trim().is_empty()) {
                return Err(format!(
                    "features `{node}`: `key` lists no properties, or an empty one"
                ));
            }
            features_key.push((node.clone(), key.clone()));
        }
    }
    Ok(Page {
        center: (center[0], center[1]),
        scale,
        dpi,
        width,
        height,
        zoom: page.zoom.or(defaults.zoom),
        out,
        format,
        features,
        features_key,
        features_coords: page
            .features_coords
            .or(defaults.features_coords)
            .unwrap_or(FeatureCoords::Lonlat),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::TileByteCache;
    use crate::{render_pages, run_frame, CommonArgs, FrameCmd};
    use ezu::core::frame::{Frame, FrameRequest};
    use std::sync::Arc;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ezu-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A network-free style over every kind of input a page reuses: vector
    /// tiles from a directory (one z14 tile, overzoomed), a GeoJSON polygon
    /// blurred across tile edges, and labels with their boxes.
    fn write_style(dir: &Path) -> PathBuf {
        let tile_dir = dir.join("tiles/14/14554");
        std::fs::create_dir_all(&tile_dir).unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../go/testdata/basemap-14-14554-6454.mvt");
        std::fs::copy(fixture, tile_dir.join("6454.pbf")).unwrap();
        let font = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
        let style = format!(
            r##"{{
              "name": "pages-test",
              "tile-size": 256,
              "pad": 8,
              "sources": {{
                "base":   {{ "type": "mvt", "url": "file:tiles/{{z}}/{{x}}/{{y}}.pbf" }},
                "shapes": {{ "type": "geojson", "data": {{ "type": "FeatureCollection", "features": [
                  {{ "type": "Feature", "properties": {{}}, "geometry": {{ "type": "Polygon",
                    "coordinates": [[[139.799, 35.628], [139.803, 35.629],
                                     [139.801, 35.632], [139.799, 35.628]]] }} }}
                ] }} }},
                "pts":    {{ "type": "geojson", "data": {{ "type": "FeatureCollection", "features": [
                  {{ "type": "Feature", "properties": {{ "name": "Alpha" }},
                    "geometry": {{ "type": "Point", "coordinates": [139.8005, 35.6305] }} }},
                  {{ "type": "Feature", "properties": {{ "name": "Beta" }},
                    "geometry": {{ "type": "Point", "coordinates": [139.8020, 35.6295] }} }}
                ] }} }},
                "body":   {{ "type": "font", "url": "file:{font}" }}
              }},
              "nodes": {{
                "bg":     {{ "op": "solid", "color": "#fbf6e6" }},
                "bldg":   {{ "op": "features", "source": "base", "layer": "buildings" }},
                "houses": {{ "op": "fill-solid", "features": "@bldg", "fill": "#99887780" }},
                "feat":   {{ "op": "features", "source": "shapes", "layer": "shapes" }},
                "fill":   {{ "op": "fill-solid", "features": "@feat", "fill": "#33669980" }},
                "soft":   {{ "op": "blur", "input": "@fill", "sigma": 3 }},
                "feats":  {{ "op": "features", "source": "pts", "layer": "pts" }},
                "labels": {{ "op": "text-labels", "features": "@feats", "source": "pts", "layer": "pts",
                             "font": ["body"], "text": ["get", "name"], "size": 16,
                             "color": "#000000" }},
                "placed": {{ "op": "label-placement", "labels": ["@labels"] }},
                "boxes":  {{ "op": "label-boxes", "placement": "@placed", "include": "all" }},
                "text":   {{ "op": "text-draw", "labels": "@labels", "placement": "@placed" }},
                "out":    {{ "op": "stack", "layers": ["@bg", "@houses", "@soft", "@text"] }}
              }},
              "output": "@out"
            }}"##,
            font = font.display().to_string().replace('\\', "/"),
        );
        let path = dir.join("style.json");
        std::fs::write(&path, style).unwrap();
        path
    }

    fn common(style: &Path) -> CommonArgs {
        CommonArgs {
            style: style.to_string_lossy().into_owned(),
            assets_dir: None,
            pmtiles: None,
            mvt: None,
            overzoom_levels: 4,
            params: Vec::new(),
            strict: false,
        }
    }

    fn page(scale: f64, out: PathBuf) -> Page {
        Page {
            center: (139.801, 35.630),
            scale,
            dpi: 96.0,
            width: 300,
            height: 200,
            zoom: None,
            out,
            format: OutputFormat::Png,
            features: Vec::new(),
            features_key: Vec::new(),
            features_coords: FeatureCoords::Lonlat,
        }
    }

    /// `page` through the single-page flags.
    async fn single(style: &Path, page: &Page) {
        run_frame(FrameCmd {
            common: common(style),
            pages: None,
            center: Some(page.center),
            scale: Some(page.scale),
            dpi: Some(page.dpi),
            size_mm: None,
            size_px: Some((page.width, page.height)),
            zoom: page.zoom,
            out: page.out.clone(),
            format: None,
            concurrency: Some(3),
            features: page.features.clone(),
            features_key: page.features_key.clone(),
            features_coords: page.features_coords,
        })
        .await
        .unwrap();
    }

    fn tile_size(scale: f64) -> u32 {
        Frame::compute(&FrameRequest {
            center_lon: 139.801,
            center_lat: 35.630,
            scale,
            dpi: 96.0,
            width: 300,
            height: 200,
            zoom: None,
            style_tile_size: 256,
        })
        .unwrap()
        .tile_size
    }

    /// Every page of a job, the features file included, is byte-identical
    /// to the same page rendered alone — across a change of tile size and
    /// back, and for a page repeated.
    #[tokio::test(flavor = "multi_thread")]
    async fn each_page_matches_its_single_page_run() {
        let dir = scratch("frame-pages");
        let style = write_style(&dir);
        assert_ne!(tile_size(5000.0), tile_size(7000.0));

        let mut first = page(5000.0, dir.join("a.png"));
        first.features = vec![("boxes".into(), dir.join("a.geojson"))];
        first.features_key = vec![("boxes".into(), vec!["label-id".into(), "part".into()])];
        first.features_coords = FeatureCoords::Px;
        let job = vec![
            first,
            page(7000.0, dir.join("b.png")),
            page(5000.0, dir.join("c.png")),
        ];
        std::fs::write(
            dir.join("pages.json"),
            r#"{ "defaults": { "center": [139.801, 35.630], "dpi": 96, "size-px": [300, 200] },
                 "pages": [
                   { "scale": 5000, "out": "a.png", "features-coords": "px",
                     "features": { "boxes": { "out": "a.geojson", "key": ["label-id", "part"] } } },
                   { "scale": 7000, "out": "b.png" },
                   { "scale": 5000, "out": "c.png" } ] }"#,
        )
        .unwrap();
        assert_eq!(read(&dir.join("pages.json")).unwrap(), job);
        run_frame(FrameCmd {
            common: common(&style),
            pages: Some(dir.join("pages.json")),
            center: None,
            scale: None,
            dpi: None,
            size_mm: None,
            size_px: None,
            zoom: None,
            out: PathBuf::from("out.png"),
            format: None,
            concurrency: Some(3),
            features: Vec::new(),
            features_key: Vec::new(),
            features_coords: FeatureCoords::Lonlat,
        })
        .await
        .unwrap();

        let alone = dir.join("alone");
        std::fs::create_dir_all(&alone).unwrap();
        for p in &job {
            let mut solo = p.clone();
            solo.out = alone.join(p.out.file_name().unwrap());
            for (_, path) in &mut solo.features {
                *path = alone.join(path.file_name().unwrap());
            }
            single(&style, &solo).await;
            let image = std::fs::read(&p.out).unwrap();
            assert!(
                image == std::fs::read(&solo.out).unwrap(),
                "{}",
                p.out.display()
            );
            for ((_, path), (_, solo_path)) in p.features.iter().zip(&solo.features) {
                let text = std::fs::read_to_string(path).unwrap();
                assert_eq!(text, std::fs::read_to_string(solo_path).unwrap());
                assert!(text.contains("Alpha") && text.contains("Beta"), "{text}");
            }
        }
        // The buildings, the polygon and the labels all reach the page.
        let a = tiny_skia::Pixmap::load_png(&job[0].out).unwrap();
        let colours: std::collections::HashSet<_> = a.data().chunks(4).collect();
        assert!(colours.len() > 20, "{} colours", colours.len());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A page over tiles an earlier page fetched reads them from memory.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_page_over_fetched_tiles_fetches_nothing() {
        let dir = scratch("frame-pages-cache");
        let style = write_style(&dir);
        let run = |pages: Vec<Page>| {
            let style = style.clone();
            async move {
                let cache = Arc::new(TileByteCache::new(64 << 20));
                render_pages(&common(&style), &pages, 3, Some(Arc::clone(&cache)))
                    .await
                    .unwrap();
                cache.counts()
            }
        };
        let (_, alone) = run(vec![page(5000.0, dir.join("a.png"))]).await;
        // z17 back to the z14 tile: three missing levels, then the one there.
        assert!(alone >= 4, "{alone} fetches");
        let (hits, misses) = run(vec![
            page(5000.0, dir.join("a.png")),
            page(5000.0, dir.join("b.png")),
        ])
        .await;
        assert_eq!(misses, alone, "the later pages fetched");
        assert!(hits > 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `--pages` stands in for the single-page flags, and a pages file's
    /// errors say which file.
    #[tokio::test]
    async fn pages_replace_the_single_page_flags() {
        use clap::Parser;
        let parse = |args: &[&str]| {
            crate::Cli::try_parse_from(
                ["ezu", "frame", "--style", "s.json"]
                    .iter()
                    .chain(args)
                    .copied(),
            )
        };
        assert!(parse(&["--pages", "p.json"]).is_ok());
        assert!(parse(&["--pages", "p.json", "--concurrency", "2"]).is_ok());
        for flag in [
            &["--center", "139.7,35.6"][..],
            &["--scale", "1000"],
            &["--dpi", "200"],
            &["--size-mm", "245x174"],
            &["--size-px", "10x10"],
            &["--zoom", "17"],
            &["--out", "a.png"],
            &["--format", "png"],
            &["--features", "boxes=a.json"],
            &["--features-key", "boxes=label-id"],
            &["--features-coords", "px"],
        ] {
            let args: Vec<&str> = ["--pages", "p.json"].iter().chain(flag).copied().collect();
            let err = parse(&args).expect_err(flag[0]);
            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::ArgumentConflict,
                "{}",
                flag[0]
            );
        }
        // Without --pages the page's own flags are required as before.
        let err = parse(&["--scale", "1000", "--dpi", "200", "--size-px", "10x10"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);

        let dir = scratch("frame-pages-errors");
        let path = dir.join("pages.json");
        std::fs::write(&path, r#"{ "pages": [ { "scale": 1000 } ] }"#).unwrap();
        let err = read(&path).unwrap_err();
        assert_eq!(
            err,
            format!(
                "pages file {}: page 1: `center` is required, on the page or in `defaults`",
                path.display()
            )
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    fn pages(text: &str) -> Result<Vec<Page>, String> {
        parse(text, Path::new("/job"))
    }

    #[test]
    fn a_page_takes_what_it_leaves_out_from_the_defaults() {
        let got = pages(
            r#"{ "defaults": { "dpi": 200, "size-mm": [245, 174], "features-coords": "px",
                               "features": { "boxes": { "out": "d.geojson" } } },
                 "pages": [
                   { "center": [139.70, 35.66], "scale": 1000, "out": "p1.png",
                     "features": { "boxes": { "out": "p1.geojson", "key": ["label-id", "part"] } } },
                   { "center": [139.71, 35.66], "scale": 2500, "zoom": 17, "out": "/abs/p2.webp",
                     "size-px": [300, 200], "dpi": 96 } ] }"#,
        )
        .unwrap();
        assert_eq!(
            got[0],
            Page {
                center: (139.70, 35.66),
                scale: 1000.0,
                dpi: 200.0,
                width: 1929,
                height: 1370,
                zoom: None,
                out: PathBuf::from("/job/p1.png"),
                format: OutputFormat::Png,
                features: vec![("boxes".into(), PathBuf::from("/job/p1.geojson"))],
                features_key: vec![("boxes".into(), vec!["label-id".into(), "part".into()])],
                features_coords: FeatureCoords::Px,
            }
        );
        // `size-px` on the page stands in for the default `size-mm`, and
        // the default `features` fill in for the page's.
        let p2 = &got[1];
        assert_eq!(
            (p2.width, p2.height, p2.dpi, p2.zoom),
            (300, 200, 96.0, Some(17))
        );
        assert_eq!(p2.out, PathBuf::from("/abs/p2.webp"));
        assert_eq!(p2.format, OutputFormat::Webp);
        assert_eq!(
            p2.features,
            vec![("boxes".into(), PathBuf::from("/job/d.geojson"))]
        );
        assert!(p2.features_key.is_empty());
    }

    #[test]
    fn mistakes_name_the_page() {
        let err = |text: &str| pages(text).unwrap_err();
        let page = r#""center": [139.7, 35.66], "scale": 1000, "dpi": 96, "size-px": [10, 10]"#;

        let e = err(&format!(
            r#"{{ "pages": [ {{ {page}, "out": "a.png" }}, {{ {page}, "centre": [0, 0], "out": "b.png" }} ] }}"#
        ));
        assert!(e.starts_with("page 2: unknown field `centre`"), "{e}");
        let e = err(&format!(r#"{{ "pages": [ {{ {page} }} ] }}"#));
        assert_eq!(e, "page 1: `out` is required");
        let e = err(&format!(
            r#"{{ "pages": [ {{ {page}, "out": "a.png" }}, {{ {page}, "out": "b.png",
                 "features": {{ "boxes": {{ "out": "a.png" }} }} }} ] }}"#
        ));
        // The path as the platform joins it: `\` between the parts on Windows.
        let a = Path::new("/job").join("a.png");
        assert_eq!(
            e,
            format!("page 2: {} is already written by page 1", a.display())
        );
        let e = err(
            r#"{ "pages": [ { "scale": 1000, "dpi": 96, "size-px": [10, 10], "out": "a.png" } ] }"#,
        );
        assert_eq!(
            e,
            "page 1: `center` is required, on the page or in `defaults`"
        );
        let e = err(&format!(
            r#"{{ "defaults": {{ "size-mm": [1, 1] }}, "pages": [ {{ {page}, "size-mm": [1, 1], "out": "a.png" }} ] }}"#
        ));
        assert_eq!(e, "page 1: `size-mm` and `size-px` are mutually exclusive");
        let e = err(r#"{ "defaults": { "out": "a.png" }, "pages": [ {} ] }"#);
        assert_eq!(e, "defaults: `out` is given per page");
        let e = err(r#"{ "default": {}, "pages": [] }"#);
        assert!(e.starts_with("unknown field `default`"), "{e}");
        assert_eq!(err(r#"{ "pages": [] }"#), "`pages` is empty");
        let e = err(&format!(
            r#"{{ "pages": [ {{ {page}, "out": "a.png", "features": {{ "boxes": {{ "out": "b.json", "key": [] }} }} }} ] }}"#
        ));
        assert_eq!(
            e,
            "page 1: features `boxes`: `key` lists no properties, or an empty one"
        );
    }
}
