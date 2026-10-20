//! `ezu frame --features`: a features node's value, gathered over every tile
//! a print frame renders, written as one GeoJSON file.
//!
//! The gathering and the file are the renderer's
//! ([`ezu::renderer::FrameFeatureCollector`]), so the Go and JS packages
//! write the same bytes; what is here is reading the flags into its request.

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::ValueEnum;
use ezu::renderer::{FrameFeatureNode, FrameFeatures};

/// Coordinate system of the written GeoJSON.
// The variants carry plain comments, not doc comments: clap would turn
// those into per-value help and switch the whole command's `--help` to
// its long layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FeatureCoords {
    // WGS84 longitude and latitude, as RFC 7946 GeoJSON expects.
    Lonlat,
    // Pixels of the output image: origin at its top-left corner, y down.
    Px,
}

impl From<FeatureCoords> for ezu::renderer::FeatureCoords {
    fn from(c: FeatureCoords) -> Self {
        match c {
            FeatureCoords::Lonlat => Self::LonLat,
            FeatureCoords::Px => Self::Px,
        }
    }
}

/// One page's `--features` and `--features-key`, as the request the
/// renderer gathers them by, and the file each of its entries is written to.
pub(crate) struct Exports {
    pub request: FrameFeatures,
    pub paths: Vec<PathBuf>,
}

/// `NODE=PATH`.
pub(crate) fn parse_node_path(s: &str) -> Result<(String, PathBuf), String> {
    match s.split_once('=') {
        Some((node, path)) if !node.is_empty() && !path.is_empty() => {
            Ok((node.to_string(), PathBuf::from(path)))
        }
        _ => Err(format!("expected NODE=PATH, got `{s}`")),
    }
}

/// `NODE=PROP[,PROP…]`.
pub(crate) fn parse_node_key(s: &str) -> Result<(String, Vec<String>), String> {
    let bad = || format!("expected NODE=PROP[,PROP...], got `{s}`");
    let (node, props) = s.split_once('=').ok_or_else(bad)?;
    let props: Vec<String> = props.split(',').map(|p| p.trim().to_string()).collect();
    if node.is_empty() || props.iter().any(String::is_empty) {
        return Err(bad());
    }
    Ok((node.to_string(), props))
}

/// Pair every `--features` with its key. Whether each names a features
/// node of the style is the renderer's to check, which it does before
/// anything renders.
pub(crate) fn exports(
    features: &[(String, PathBuf)],
    keys: &[(String, Vec<String>)],
    coords: FeatureCoords,
) -> Result<Exports, String> {
    let mut key_of: BTreeMap<&str, &Vec<String>> = BTreeMap::new();
    for (node, props) in keys {
        if !features.iter().any(|(n, _)| n == node) {
            return Err(format!(
                "--features-key names `{node}`, which no --features writes"
            ));
        }
        if key_of.insert(node, props).is_some() {
            return Err(format!("--features-key is given twice for `{node}`"));
        }
    }
    Ok(Exports {
        request: FrameFeatures {
            nodes: features
                .iter()
                .map(|(node, _)| FrameFeatureNode {
                    node: node.clone(),
                    key: key_of.get(node.as_str()).map(|k| (*k).clone()),
                })
                .collect(),
            coords: coords.into(),
        },
        paths: features.iter().map(|(_, path)| path.clone()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{run_frame, CommonArgs, FrameCmd};
    use ezu::core::coord::{lat_to_world_y, lon_to_world_x, world_x_to_lon, world_y_to_lat};
    use ezu::core::frame::{Frame, FrameRequest};
    use std::path::Path;

    /// 1:5000 at 96 dpi: tiles of ~188 px at z=17, and a 300×200 frame
    /// that straddles tiles on every side.
    fn request() -> FrameRequest {
        FrameRequest {
            center_lon: 139.70,
            center_lat: 35.66,
            scale: 5000.0,
            dpi: 96.0,
            width: 300,
            height: 200,
            zoom: None,
            style_tile_size: 256,
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ezu-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn font_url() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
        format!("file:{}", path.display()).replace('\\', "/")
    }

    /// `lon,lat` of output-image pixel `(x, y)` of `frame`.
    fn lonlat_at(frame: &Frame, x: f64, y: f64) -> (f64, f64) {
        let world = frame.tile_size as f64 * (1u64 << frame.zoom) as f64;
        (
            world_x_to_lon((x + frame.left as f64) / world),
            world_y_to_lat((y + frame.top as f64) / world),
        )
    }

    /// Point features as GeoJSON, each `(name, lon, lat)`.
    fn points(pts: &[(&str, f64, f64)]) -> String {
        let feats: Vec<String> = pts
            .iter()
            .map(|(name, lon, lat)| {
                format!(
                    r#"{{ "type": "Feature", "properties": {{ "name": "{name}" }},
                         "geometry": {{ "type": "Point", "coordinates": [{lon}, {lat}] }} }}"#
                )
            })
            .collect();
        format!(
            r#"{{ "type": "FeatureCollection", "features": [{}] }}"#,
            feats.join(",")
        )
    }

    async fn frame(
        style: &str,
        dir: &Path,
        features: &[(&str, &Path)],
        key: Option<&str>,
        coords: FeatureCoords,
    ) -> Result<(), String> {
        let style_path = dir.join("style.json");
        std::fs::write(&style_path, style).unwrap();
        let req = request();
        run_frame(FrameCmd {
            common: CommonArgs {
                style: style_path.to_string_lossy().into_owned(),
                assets_dir: None,
                pmtiles: None,
                mvt: None,
                overzoom_levels: 4,
                params: Vec::new(),
                strict: false,
                pad: None,
            },
            pages: None,
            center: Some((req.center_lon, req.center_lat)),
            scale: Some(req.scale),
            dpi: Some(req.dpi),
            size_mm: None,
            size_px: Some((req.width, req.height)),
            zoom: None,
            out: dir.join("page.png"),
            format: None,
            concurrency: Some(3),
            features: features
                .iter()
                .map(|(n, p)| (n.to_string(), p.to_path_buf()))
                .collect(),
            features_key: key
                .map(|k| vec![parse_node_key(k).unwrap()])
                .unwrap_or_default(),
            features_coords: coords,
        })
        .await
        .map_err(|e| e.to_string())
    }

    fn read(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn features(v: &serde_json::Value) -> &Vec<serde_json::Value> {
        v["features"].as_array().unwrap()
    }

    /// Black labels on white, with every `label-boxes` box exported.
    fn label_style(pts: &str) -> String {
        format!(
            r##"{{
              "name": "frame-labels",
              "tile-size": 256,
              "sources": {{
                "pts":  {{ "type": "geojson", "data": {pts} }},
                "body": {{ "type": "font", "url": "{font}" }}
              }},
              "nodes": {{
                "bg":     {{ "op": "solid", "color": "#ffffff" }},
                "feats":  {{ "op": "features", "source": "pts", "layer": "pts" }},
                "labels": {{ "op": "text-labels", "features": "@feats", "source": "pts", "layer": "pts",
                             "font": ["body"], "text": ["get", "name"], "size": 16,
                             "color": "#000000" }},
                "placed": {{ "op": "label-placement", "labels": ["@labels"] }},
                "boxes":  {{ "op": "label-boxes", "placement": "@placed", "include": "all" }},
                "text":   {{ "op": "text-draw", "labels": "@labels", "placement": "@placed" }},
                "out":    {{ "op": "stack", "layers": ["@bg", "@text"] }}
              }},
              "output": "@out"
            }}"##,
            font = font_url(),
        )
    }

    /// Labels straddling a tile edge inside the frame are listed once each
    /// with the label-boxes key, the file is the same on every run, and the
    /// text boxes sit where the text is drawn.
    #[tokio::test(flavor = "multi_thread")]
    async fn label_boxes_across_tile_edges_are_listed_once() {
        let dir = scratch("frame-labels");
        let fr = Frame::compute(&request()).unwrap();
        assert!(fr.tiles_x.len() >= 3);
        // The first tile edge inside the frame, and a row in its middle.
        let (edge, _) = fr.tile_offset(fr.tiles_x.start + 1, fr.tiles_y.start);
        let edge = edge as f64;
        let pts = [("Alpha", edge - 3.0, 60.0), ("Beta", edge + 4.0, 140.0)];
        let geo: Vec<(&str, f64, f64)> = pts
            .iter()
            .map(|&(n, x, y)| {
                let (lon, lat) = lonlat_at(&fr, x, y);
                (n, lon, lat)
            })
            .collect();
        let style = label_style(&points(&geo));

        let (a, b, raw) = (dir.join("a.json"), dir.join("b.json"), dir.join("raw.json"));
        let key = Some("boxes=label-id,part");
        frame(&style, &dir, &[("boxes", &a)], key, FeatureCoords::Px)
            .await
            .unwrap();
        frame(&style, &dir, &[("boxes", &b)], key, FeatureCoords::Px)
            .await
            .unwrap();
        frame(&style, &dir, &[("boxes", &raw)], None, FeatureCoords::Px)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());

        let key_of = |f: &serde_json::Value| {
            let p = &f["properties"];
            (
                p["label-id"].as_i64().unwrap(),
                p["part"].as_str().unwrap().to_string(),
            )
        };
        let doc = read(&a);
        let keyed: Vec<_> = features(&doc).iter().map(key_of).collect();
        let mut sorted = keyed.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(keyed, sorted, "each (label-id, part) once, in key order");
        // Both tiles beside the edge saw both labels: without the key the
        // copies are all there.
        let raw_doc = read(&raw);
        assert!(features(&raw_doc).len() > features(&doc).len());

        let image = tiny_skia::Pixmap::load_png(dir.join("page.png")).unwrap();
        let dark = |x: u32, y: u32| image.pixel(x, y).is_some_and(|p| p.red() < 128);
        for (name, ax, ay) in pts {
            let text = features(&doc)
                .iter()
                .find(|f| f["properties"]["name"] == name && f["properties"]["part"] == "text")
                .unwrap_or_else(|| panic!("a text box for {name}"));
            assert_eq!(text["properties"]["placed"], true);
            let ring = text["geometry"]["coordinates"][0].as_array().unwrap();
            let xs: Vec<f64> = ring.iter().map(|c| c[0].as_f64().unwrap()).collect();
            let ys: Vec<f64> = ring.iter().map(|c| c[1].as_f64().unwrap()).collect();
            let fold = |v: &[f64], f: fn(f64, f64) -> f64, init| v.iter().copied().fold(init, f);
            let (x0, x1) = (fold(&xs, f64::min, f64::MAX), fold(&xs, f64::max, f64::MIN));
            let (y0, y1) = (fold(&ys, f64::min, f64::MAX), fold(&ys, f64::max, f64::MIN));
            // Centred on the anchor, and across the tile edge.
            assert!((0.5 * (x0 + x1) - ax).abs() < 2.0, "{name}: {x0}..{x1}");
            assert!((0.5 * (y0 + y1) - ay).abs() < 3.0, "{name}: {y0}..{y1}");
            assert!(x0 < edge && x1 > edge, "{name} straddles the edge");
            // The text's ink lies inside its box, and there is ink.
            let mut ink = 0;
            for y in (y0 - 6.0) as u32..(y1 + 6.0) as u32 {
                for x in (x0 - 6.0) as u32..(x1 + 6.0) as u32 {
                    if dark(x, y) {
                        ink += 1;
                        let inside = (x as f64) >= x0 - 1.0
                            && (x as f64) <= x1 + 1.0
                            && (y as f64) >= y0 - 1.0
                            && (y as f64) <= y1 + 1.0;
                        assert!(inside, "{name}: ink at ({x}, {y}) outside its box");
                    }
                }
            }
            assert!(ink > 20, "{name}: drawn");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A point written as lon/lat comes back where it was declared, and as
    /// px where the frame puts it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_point_round_trips_through_lonlat_and_px() {
        let dir = scratch("frame-point");
        let fr = Frame::compute(&request()).unwrap();
        let (lon, lat) = (139.7012345, 35.6598765);
        let style = format!(
            r##"{{
              "name": "frame-point",
              "tile-size": 256,
              "sources": {{ "pts": {{ "type": "geojson", "data": {pts} }} }},
              "nodes": {{
                "feats": {{ "op": "features", "source": "pts", "layer": "pts" }},
                "dots":  {{ "op": "circles", "features": "@feats", "radius": 3, "color": "#000000" }}
              }},
              "output": "@dots"
            }}"##,
            pts = points(&[("p", lon, lat)]),
        );
        let (geo, px) = (dir.join("geo.json"), dir.join("px.json"));
        let key = Some("feats=name");
        frame(&style, &dir, &[("feats", &geo)], key, FeatureCoords::Lonlat)
            .await
            .unwrap();
        frame(&style, &dir, &[("feats", &px)], key, FeatureCoords::Px)
            .await
            .unwrap();

        let doc = read(&geo);
        let [f] = features(&doc).as_slice() else {
            panic!("one feature: {doc}");
        };
        assert_eq!(f["geometry"]["type"], "Point");
        let c = &f["geometry"]["coordinates"];
        // Tile geometry is quantised to 1/4096 of a ~188 px tile.
        assert!((c[0].as_f64().unwrap() - lon).abs() < 2e-6, "{c}");
        assert!((c[1].as_f64().unwrap() - lat).abs() < 2e-6, "{c}");

        let doc = read(&px);
        let c = &features(&doc)[0]["geometry"]["coordinates"];
        let world = fr.tile_size as f64 * (1u64 << fr.zoom) as f64;
        let want = (
            lon_to_world_x(lon) * world - fr.left as f64,
            lat_to_world_y(lat) * world - fr.top as f64,
        );
        assert!(
            (c[0].as_f64().unwrap() - want.0).abs() < 0.1,
            "{c} vs {want:?}"
        );
        assert!(
            (c[1].as_f64().unwrap() - want.1).abs() < 0.1,
            "{c} vs {want:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn only_features_nodes_can_be_written() {
        let dir = scratch("frame-features-errors");
        let style = label_style(&points(&[]));
        let out = dir.join("x.json");
        let err = frame(&style, &dir, &[("nope", &out)], None, FeatureCoords::Lonlat)
            .await
            .unwrap_err();
        assert!(err.contains("no node `nope`"), "{err}");
        let err = frame(&style, &dir, &[("bg", &out)], None, FeatureCoords::Lonlat)
            .await
            .unwrap_err();
        assert!(err.contains("`bg` produces raster"), "{err}");
        let err = frame(
            &style,
            &dir,
            &[("boxes", &out)],
            Some("feats=name"),
            FeatureCoords::Lonlat,
        )
        .await
        .unwrap_err();
        assert!(err.contains("no --features writes"), "{err}");
        assert!(!out.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn flags_parse() {
        assert_eq!(
            parse_node_path("boxes=out/labels.geojson"),
            Ok(("boxes".into(), PathBuf::from("out/labels.geojson")))
        );
        assert!(parse_node_path("boxes").is_err());
        assert!(parse_node_path("=x.geojson").is_err());
        assert_eq!(
            parse_node_key("boxes=label-id,part"),
            Ok(("boxes".into(), vec!["label-id".into(), "part".into()]))
        );
        assert!(parse_node_key("boxes=label-id,").is_err());
    }
}
