//! axum handlers for `ezu serve`.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, Response,
    },
    routing::get,
    Json, Router,
};
use ezu::core::TileId as CoreTileId;
use ezu::features::mvt;
use ezu::renderer::{OutputFormat, RenderOptions};
use futures::stream::{self, Stream};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::broadcast;

use super::state::{validate_text, AppState, StyleSnapshot};
use crate::render::MissingTile;
use crate::source::{FeatureSource, SourceError};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/style", get(get_style).put(put_style))
        .route("/style/params", get(get_params_schema))
        .route("/style/attribution", get(get_attribution))
        .route("/config", get(get_config))
        .route("/style/validate", axum::routing::post(post_validate))
        .route("/style/fetch", get(get_style_fetch))
        .route("/style/events", get(get_style_events))
        .route("/schemas/ezu-style.json", get(get_schema))
        .route("/tiles/{z}/{x}/{y_ext}", get(get_tile))
        .route("/mvt/{z}/{x}/{y}", get(get_mvt))
        .route("/mvt-meta/{z}/{x}/{y}", get(get_mvt_meta))
}

async fn index() -> Html<&'static str> {
    Html(include_str!("editor.html"))
}

/// Zoom the editor should stop requesting tiles at when the source
/// declares its own depth. A source that stays quiet about it (a bare
/// `{z}/{x}/{y}` template) gets [`ABSOLUTE_MAXZOOM`] instead — the
/// renderer is happy to draw whatever the fetcher hands it, so an
/// unknown depth is no reason to fence the map in.
const ABSOLUTE_MAXZOOM: u8 = 22;

/// Server-side knobs the editor needs to configure its map. Kept as one
/// endpoint so the front end never has to guess at CLI defaults.
async fn get_config(State(s): State<AppState>) -> Json<serde_json::Value> {
    // The deepest any source goes, when every one of them says.
    let data_maxzoom = s
        .feature_sources
        .iter()
        .map(|src| src.max_zoom())
        .collect::<Option<Vec<u8>>>()
        .and_then(|zooms| zooms.into_iter().max());
    // Past the source's own depth the tile handler reprojects ancestor
    // tiles, `overzoom_levels` of them, so that is exactly how much
    // deeper than the data the editor can still ask for.
    let overzoom_levels = s.sources.overzoom_levels;
    let raster_maxzoom = match data_maxzoom {
        Some(z) => z.saturating_add(overzoom_levels).min(ABSOLUTE_MAXZOOM),
        None => ABSOLUTE_MAXZOOM,
    };
    Json(serde_json::json!({
        "overzoomLevels": overzoom_levels,
        "dataMaxzoom": data_maxzoom,
        "rasterMaxzoom": raster_maxzoom,
    }))
}

async fn get_style(State(s): State<AppState>) -> Response {
    let snap = s.style.read().await;
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(snap.text.clone()))
        .expect("response builder with valid headers + body never fails")
}

async fn put_style(
    State(s): State<AppState>,
    body: String,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let next_version = { s.style.read().await.version + 1 };
    let snap = StyleSnapshot::build(body, next_version, &s.sources)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let version = snap.version;
    *s.style.write().await = snap;
    Ok(Json(json!({ "version": version })))
}

/// JSON Schema for the current style's parameter values — what the
/// tile endpoint accepts as query-string overrides. Editor UIs can
/// build sliders / color pickers / toggles straight off this.
async fn get_params_schema(State(s): State<AppState>) -> Json<serde_json::Value> {
    let snap = s.style.read().await;
    Json(snap.doc.params_schema())
}

/// Effective attribution for the current style: the document's own
/// declarations merged with upstream metadata (TileJSON `attribution`,
/// PMTiles archive metadata) inherited by sources that declare none.
/// The editor feeds this to the MapLibre attribution control.
async fn get_attribution(State(s): State<AppState>) -> Json<serde_json::Value> {
    let snap = s.style.read().await;
    let mut list: Vec<String> = snap
        .doc
        .attributions()
        .iter()
        .map(|a| a.to_string())
        .collect();
    let mut push = |a: String| {
        if !a.is_empty() && !list.contains(&a) {
            list.push(a);
        }
    };
    // DEM sources first, then imagery, each inheriting upstream metadata
    // only when the document's own entry declares no attribution.
    let pyramids = snap.tiles.pyramids();
    for dem in [true, false] {
        for src in pyramids.iter().filter(|p| p.is_dem == dem) {
            let declares = snap
                .doc
                .sources
                .get(&*src.name)
                .is_some_and(|d| d.attribution().is_some());
            if let (false, Some(a)) = (declares, src.attribution()) {
                push(a.to_string());
            }
        }
    }
    // Each MVT/PMTiles feature source inherits upstream metadata only
    // when the document's own entry doesn't declare attribution.
    for src in s.feature_sources.iter() {
        let declares = snap
            .doc
            .sources
            .get(&*src.name)
            .is_some_and(|d| d.attribution().is_some());
        if !declares {
            if let Some(a) = src.attribution() {
                push(a.to_string());
            }
        }
    }
    Json(json!({ "attributions": list, "attribution": list.join(" | ") }))
}

/// Dry-run the parse + graph-build pipeline `PUT /style` would run,
/// without prefetching URL assets or swapping the live snapshot. The
/// live editor pings this on every keystroke (debounced), so it has
/// to be cheap.
async fn post_validate(body: String) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    validate_text(&body).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

/// Fetch a remote style document on behalf of the editor. Limited to
/// http(s) URLs so this can't be coaxed into serving arbitrary local
/// files via the browser. Used by the "Open URL" button.
async fn get_style_fetch(
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    let url = q.get("url").ok_or((
        StatusCode::BAD_REQUEST,
        "missing url query parameter".into(),
    ))?;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err((
            StatusCode::BAD_REQUEST,
            "only http(s) URLs are allowed".into(),
        ));
    }
    let text = crate::fetch_text(url)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(text))
        .expect("response builder with valid headers + body never fails"))
}

/// Server-Sent Events stream of style-reload notifications. Fires
/// whenever the local style file watched by `ezu serve <file>` changes
/// on disk and the server successfully rebuilt the snapshot. The
/// editor listens to this and either silently swaps the buffer (clean
/// editor) or surfaces a banner (when the user has unsaved edits).
async fn get_style_events(
    State(s): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let rx = s.events.subscribe();
    let stream = stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let event = Event::default()
                        .event("reload")
                        .json_data(&ev)
                        .unwrap_or_else(|_| Event::default());
                    return Some((Ok(event), rx));
                }
                // Skip lagged events; an editor missing a couple of
                // mid-flight reloads just sees the latest one next.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

async fn get_schema(State(s): State<AppState>) -> Response {
    // Schema is derived from the live node registry so it always matches
    // the ops the server can actually evaluate.
    let body = serde_json::to_vec_pretty(&*s.schema).unwrap_or_default();
    Response::builder()
        .header(header::CONTENT_TYPE, "application/schema+json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .expect("response builder with valid headers + body never fails")
}

#[derive(Clone, Copy)]
enum TileFormat {
    Png,
    Webp,
}

impl TileFormat {
    fn content_type(self) -> &'static str {
        match self {
            TileFormat::Png => "image/png",
            TileFormat::Webp => "image/webp",
        }
    }

    fn encoding(self) -> OutputFormat {
        match self {
            TileFormat::Png => OutputFormat::Png,
            TileFormat::Webp => OutputFormat::Webp,
        }
    }
}

/// The status a failed tile answers with: 404 when the style asked for a
/// missing source tile to fail the render (`on-missing: error`), 502 when a
/// source could not be fetched, and 500 when the render itself failed.
fn status_of(e: &(dyn std::error::Error + 'static)) -> StatusCode {
    if e.downcast_ref::<MissingTile>().is_some() {
        StatusCode::NOT_FOUND
    } else if e.downcast_ref::<SourceError>().is_some() {
        StatusCode::BAD_GATEWAY
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

async fn get_tile(
    State(s): State<AppState>,
    Path((z, x, y_ext)): Path<(u8, u32, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    // Sniff the output format off the extension. Default is PNG so the
    // legacy `/tiles/{z}/{x}/{y}` (no suffix) and `.png` keep working.
    let (y_str, format) = if let Some(s) = y_ext.strip_suffix(".webp") {
        (s, TileFormat::Webp)
    } else if let Some(s) = y_ext.strip_suffix(".png") {
        (s, TileFormat::Png)
    } else {
        (y_ext.as_str(), TileFormat::Png)
    };
    let y: u32 = y_str
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "bad y".into()))?;
    let tile = CoreTileId::new(z, x, y);

    // Take only what we need from the snapshot to keep the lock window
    // short. Query-string parameter overrides are validated against
    // the document's `params` declarations while we hold the lock.
    let (tiles, params) = {
        let snap = s.style.read().await;
        let mut params = Vec::new();
        for (name, raw) in &q {
            // `v` is reserved as the editor's cache-busting version key.
            if name == "v" {
                continue;
            }
            ezu::graph::parse_param_value(&snap.doc.params, name, raw)
                .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
            params.push((name.clone(), raw.clone()));
        }
        (Arc::clone(&snap.tiles), params)
    };

    let bytes = tiles
        .render(tile, move |r, set, opts| {
            let opts = RenderOptions {
                format: format.encoding(),
                params,
                ..opts
            };
            r.render_tile_with(tile.z, tile.x, tile.y, set, opts)
        })
        .await
        .map_err(|e| {
            tracing::error!("tile {z}/{x}/{}: {e}", tile.y);
            (status_of(e.as_ref()), e.to_string())
        })?;

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, format.content_type())
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(bytes))
        .expect("response builder with valid headers + body never fails"))
}

/// The feature source a raw-MVT request names with `?source=`, or the
/// style's first one.
fn requested_source<'s>(
    s: &'s AppState,
    q: &HashMap<String, String>,
) -> Result<Option<&'s FeatureSource>, (StatusCode, String)> {
    match q.get("source") {
        Some(name) => s
            .feature_sources
            .get(Some(name))
            .map(Some)
            .ok_or((StatusCode::NOT_FOUND, format!("no feature source `{name}`"))),
        None => Ok(s.feature_sources.get(None)),
    }
}

/// Return raw decompressed MVT bytes for `(z, x, y)` from the feature
/// source named by `?source=` (default: the first). Used by the WASM demo,
/// which does its own decoding + rendering client-side.
async fn get_mvt(
    State(s): State<AppState>,
    Path((z, x, y)): Path<(u8, u32, u32)>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    let tile = CoreTileId::new(z, x, y);
    let Some(source) = requested_source(&s, &q)? else {
        return Err((StatusCode::NOT_FOUND, "tile not in source".into()));
    };
    // This endpoint serves native MVT bytes that callers expect to
    // decode at the requested tile's coordinate frame, so a parent
    // fallback (different coords) would be wrong. Treat overzoom hits
    // as misses here.
    let Some((bytes, src)) = fetch_mvt(&s, source, tile).await? else {
        return Err((StatusCode::NOT_FOUND, "tile not in source".into()));
    };
    if src != tile {
        return Err((StatusCode::NOT_FOUND, "tile not in source".into()));
    }
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/vnd.mapbox-vector-tile")
        .header(header::CACHE_CONTROL, "public, max-age=300")
        .body(Body::from(bytes.to_vec()))
        .expect("response builder with valid headers + body never fails"))
}

/// Decode the MVT for `(z, x, y)` and report which layers it contains.
/// Used by the editor's inspect panel to populate per-layer toggles
/// without having to re-decode MVTs in the browser.
async fn get_mvt_meta(
    State(s): State<AppState>,
    Path((z, x, y)): Path<(u8, u32, u32)>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let tile = CoreTileId::new(z, x, y);
    let Some(source) = requested_source(&s, &q)? else {
        return Ok(Json(json!({ "layers": [] })));
    };
    // Layer names + geometry kinds don't change under overzoom, so it's
    // fine if the bytes came from an ancestor — no clipping needed.
    let Some((bytes, _src)) = fetch_mvt(&s, source, tile).await? else {
        return Ok(Json(json!({ "layers": [] })));
    };
    let decoded =
        mvt::decode(&bytes).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let layers: Vec<_> = decoded
        .layers
        .iter()
        .map(|l| {
            let (mut p, mut ln, mut pg) = (false, false, false);
            for f in &l.features {
                if !f.geometry.points.is_empty() {
                    p = true;
                }
                if !f.geometry.lines.is_empty() {
                    ln = true;
                }
                if !f.geometry.polygons.is_empty() {
                    pg = true;
                }
            }
            let mut geoms: Vec<&str> = Vec::new();
            if p {
                geoms.push("point");
            }
            if ln {
                geoms.push("line");
            }
            if pg {
                geoms.push("polygon");
            }
            json!({
                "name": l.name,
                "geometry_types": geoms,
                "features": l.features.len(),
            })
        })
        .collect();
    Ok(Json(json!({ "layers": layers })))
}

/// Fetch `source`'s MVT for `tile`: nothing below the source's
/// `min-zoom`, the covering ancestor past its `max-zoom`, and on a miss
/// up to `overzoom_levels` parents further. The returned `CoreTileId`
/// identifies which tile the bytes actually came from — equal to `tile`
/// on a direct hit, an ancestor on overzoom. Callers that need the raw
/// native bytes (the `/mvt` endpoint) should treat a non-matching ID as a
/// miss.
async fn fetch_mvt(
    s: &AppState,
    source: &FeatureSource,
    tile: CoreTileId,
) -> Result<Option<(bytes::Bytes, CoreTileId)>, (StatusCode, String)> {
    source
        .fetch(tile, s.sources.overzoom_levels)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))
}

#[cfg(test)]
mod tests {
    use std::path::{Path as FsPath, PathBuf};

    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;
    use crate::serve::state::Sources;
    use crate::source::{FeatureSources, TileByteCache};

    fn testdata(name: &str) -> Vec<u8> {
        let path = FsPath::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../go/testdata")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// A fresh directory for one test's tiles.
    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ezu-serve-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_tile(root: &FsPath, dir: &str, (z, x, y): (u8, u32, u32), ext: &str, bytes: &[u8]) {
        let d = root.join(dir).join(z.to_string()).join(x.to_string());
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(format!("{y}.{ext}")), bytes).unwrap();
    }

    /// What `ezu serve` holds for `style`, its files under `root`.
    async fn serve(style: &str, root: &FsPath) -> Result<AppState, String> {
        let doc = ezu::style::Document::from_json(style).map_err(|e| e.to_string())?;
        let byte_cache = Arc::new(TileByteCache::new(1 << 20));
        let feature_sources = FeatureSources::open(&doc, None, root)
            .await
            .map_err(|e| e.to_string())?
            .with_byte_cache(&byte_cache, true);
        let sources = Sources {
            assets_dir: root.to_path_buf(),
            feature_sources: Arc::new(feature_sources),
            overzoom_levels: 0,
            byte_cache,
        };
        let snapshot = StyleSnapshot::build(style.to_string(), 1, &sources)
            .await
            .map_err(|e| e.to_string())?;
        Ok(AppState::new(sources, snapshot))
    }

    async fn request(
        state: &AppState,
        method: &str,
        uri: &str,
        body: &str,
    ) -> (StatusCode, Vec<u8>) {
        let response = router()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, bytes.to_vec())
    }

    /// Water stitched across tile borders, so the graph reads the
    /// neighbours.
    const STITCHED: &str = r##"{
      "name": "stitched",
      "tile-size": 64,
      "sources": { "bg": { "type": "mvt", "url": "file:bg/{z}/{x}/{y}.mvt" } },
      "nodes": {
        "f": { "op": "features", "source": "bg", "layer": "water", "stitch": "merge" },
        "fill": { "op": "fill-solid", "features": "@f", "fill": "#204080" },
        "out": { "op": "blur", "input": "@fill", "sigma": 3.0 }
      },
      "output": "@out"
    }"##;

    #[tokio::test]
    async fn a_neighbour_that_does_not_decode_is_left_out() {
        let root = scratch("neighbour");
        let centre = (14, 14554, 6454);
        write_tile(
            &root,
            "bg",
            centre,
            "mvt",
            &testdata("basemap-14-14554-6454.mvt"),
        );
        let state = serve(STITCHED, &root).await.unwrap();
        let uri = "/tiles/14/14554/6454.png";

        let (status, alone) = request(&state, "GET", uri, "").await;
        assert_eq!(status, StatusCode::OK);
        // A broken neighbour draws as a missing one.
        write_tile(&root, "bg", (14, 14555, 6454), "mvt", b"not a tile");
        let (status, beside) = request(&state, "GET", uri, "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(alone, beside);
        // A broken centre still fails the tile.
        write_tile(&root, "bg", centre, "mvt", b"not a tile either");
        let (status, _) = request(&state, "GET", uri, "").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        std::fs::remove_dir_all(&root).ok();
    }
}
