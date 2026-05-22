use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::{Html, Response},
    routing::get,
    Json, Router,
};
use ezu::core::TileId as CoreTileId;
use ezu::graph::{CanvasInfo, Evaluator, ParamValues, PortValue, TileId};
use ezu::features::mvt;
use ezu::paint::host::{raster_to_png, raster_to_webp, BrushBankLoader, TileLoader};
use serde_json::json;

use crate::state::{AppState, StyleSnapshot};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/style", get(get_style).put(put_style))
        .route("/schemas/ezu-style.json", get(get_schema))
        .route("/tiles/{z}/{x}/{y_png}", get(get_tile))
        .route("/mvt/{z}/{x}/{y}", get(get_mvt))
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

    let mvt = match s.mvt_cache.get(&tile).map(|r| r.clone()) {
        Some(b) => Some(b),
        None => match s.archive.get_tile(tile).await {
            Ok(Some(b)) => {
                s.mvt_cache.insert(tile, b.clone());
                Some(b)
            }
            Ok(None) => None,
            Err(e) => return Err((StatusCode::BAD_GATEWAY, e.to_string())),
        },
    };

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
        move || render_tile(&graph, &cache, &assets, mvt.as_deref(), tile, tile_size, pad, format)
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
    let mvt = match s.mvt_cache.get(&tile).map(|r| r.clone()) {
        Some(b) => Some(b),
        None => match s.archive.get_tile(tile).await {
            Ok(Some(b)) => {
                s.mvt_cache.insert(tile, b.clone());
                Some(b)
            }
            Ok(None) => None,
            Err(e) => return Err((StatusCode::BAD_GATEWAY, e.to_string())),
        },
    };
    let Some(bytes) = mvt else {
        return Err((StatusCode::NOT_FOUND, "tile not in archive".into()));
    };
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/vnd.mapbox-vector-tile")
        .header(header::CACHE_CONTROL, "public, max-age=300")
        .body(Body::from(bytes.to_vec()))
        .unwrap())
}

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
    s = s.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(tile.z as u64);
    s = s.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(tile.x as u64);
    s = s.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(tile.y as u64);
    s
}
