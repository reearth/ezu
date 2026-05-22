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
use ezu::graph::{CanvasInfo, Evaluator, ParamValues, PortValue, TileId};
use ezu::paint::host::{raster_to_png, raster_to_webp, BrushBankLoader, TileLoader};
use futures::stream::{self, Stream};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::broadcast;

use super::state::{validate_text, AppState, StyleSnapshot};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/style", get(get_style).put(put_style))
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

async fn get_style(State(s): State<AppState>) -> Response {
    let snap = s.style.read().await;
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(snap.text.clone()))
        .unwrap()
}

async fn put_style(
    State(s): State<AppState>,
    body: String,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let next_version = { s.style.read().await.version + 1 };
    let snap = StyleSnapshot::build(body, next_version, &s.assets_dir)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let version = snap.version;
    *s.style.write().await = snap;
    Ok(Json(json!({ "version": version })))
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
        .unwrap())
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
        .unwrap()
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
}

async fn get_tile(
    State(s): State<AppState>,
    Path((z, x, y_ext)): Path<(u8, u32, String)>,
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

    let mvt = fetch_mvt(&s, tile).await?;

    // Take only what we need from the snapshot to keep the lock window short.
    let (graph, cache, assets, tile_size, pad) = {
        let snap = s.style.read().await;
        (
            Arc::clone(&snap.graph),
            Arc::clone(&snap.cache),
            Arc::clone(&snap.assets),
            snap.doc.tile_size,
            snap.doc.pad,
        )
    };

    let bytes = tokio::task::spawn_blocking({
        move || {
            render_tile(
                &graph,
                &cache,
                &assets,
                mvt.as_deref(),
                tile,
                tile_size,
                pad,
                format,
            )
        }
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, format.content_type())
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(bytes))
        .unwrap())
}

/// Return raw decompressed MVT bytes for `(z, x, y)`. Used by the WASM demo,
/// which does its own decoding + rendering client-side.
async fn get_mvt(
    State(s): State<AppState>,
    Path((z, x, y)): Path<(u8, u32, u32)>,
) -> Result<Response, (StatusCode, String)> {
    let tile = CoreTileId::new(z, x, y);
    let mvt = fetch_mvt(&s, tile).await?;
    let Some(bytes) = mvt else {
        return Err((StatusCode::NOT_FOUND, "tile not in source".into()));
    };
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/vnd.mapbox-vector-tile")
        .header(header::CACHE_CONTROL, "public, max-age=300")
        .body(Body::from(bytes.to_vec()))
        .unwrap())
}

/// Decode the MVT for `(z, x, y)` and report which layers it contains.
/// Used by the editor's inspect panel to populate per-layer toggles
/// without having to re-decode MVTs in the browser.
async fn get_mvt_meta(
    State(s): State<AppState>,
    Path((z, x, y)): Path<(u8, u32, u32)>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let tile = CoreTileId::new(z, x, y);
    let Some(bytes) = fetch_mvt(&s, tile).await? else {
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

async fn fetch_mvt(
    s: &AppState,
    tile: CoreTileId,
) -> Result<Option<bytes::Bytes>, (StatusCode, String)> {
    if let Some(b) = s.mvt_cache.get(&tile).map(|r| r.clone()) {
        return Ok(Some(b));
    }
    match s.source.fetch(tile).await {
        Ok(Some(b)) => {
            s.mvt_cache.insert(tile, b.clone());
            Ok(Some(b))
        }
        Ok(None) => Ok(None),
        Err(e) => Err((StatusCode::BAD_GATEWAY, e.to_string())),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_tile(
    graph: &ezu::graph::Graph,
    cache: &ezu::graph::Cache,
    assets: &BrushBankLoader,
    mvt_bytes: Option<&[u8]>,
    tile: CoreTileId,
    tile_size: u32,
    pad: u32,
    format: TileFormat,
) -> Result<Vec<u8>, String> {
    let tile_id = TileId {
        z: tile.z,
        x: tile.x,
        y: tile.y,
    };
    let mut tile_loader = TileLoader::new(assets, tile_id);
    if let Some(bytes) = mvt_bytes {
        tile_loader.bind_mvt(mvt::decode(bytes).map_err(|e| format!("mvt decode: {e}"))?);
    }
    let ev = Evaluator::new(graph, cache, &tile_loader);
    let out = ev
        .render(
            tile_id,
            CanvasInfo { tile_size, pad },
            &ParamValues::new(),
            tile_seed(tile),
        )
        .map_err(|e| format!("render: {e}"))?;
    let raster = match out {
        PortValue::Raster(r) => r,
        other => return Err(format!("expected Raster output, got {:?}", other.kind())),
    };
    match format {
        TileFormat::Png => raster_to_png(&raster, tile_size, pad).map_err(|e| format!("png: {e}")),
        TileFormat::Webp => {
            raster_to_webp(&raster, tile_size, pad).map_err(|e| format!("webp: {e}"))
        }
    }
}

fn tile_seed(tile: CoreTileId) -> u64 {
    let mut s = 0u64;
    s = s
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(tile.z as u64);
    s = s
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(tile.x as u64);
    s = s
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(tile.y as u64);
    s
}
