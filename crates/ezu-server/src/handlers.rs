use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use ezu::core::TileId;
use ezu::mvt;
use ezu::paint::{self, canvas_from_style, render_style, Brush};
use ezu::style::Style;
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
    let parsed = Style::from_json(&body).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let mut snap = s.style.write().await;
    snap.version += 1;
    snap.parsed = parsed;
    snap.text = body;
    Ok(Json(json!({ "version": snap.version })))
}

async fn get_schema(State(s): State<AppState>) -> Response {
    match tokio::fs::read(&s.schema_path).await {
        Ok(bytes) => Response::builder()
            .header(header::CONTENT_TYPE, "application/schema+json")
            .body(Body::from(bytes))
            .unwrap(),
        Err(_) => (StatusCode::NOT_FOUND, "schema not found").into_response(),
    }
}

async fn get_tile(
    State(s): State<AppState>,
    Path((z, x, y_png)): Path<(u8, u32, String)>,
) -> Result<Response, (StatusCode, String)> {
    let y_str = y_png.strip_suffix(".png").unwrap_or(&y_png);
    let y: u32 = y_str
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "bad y".into()))?;
    let tile = TileId::new(z, x, y);

    // Fetch (or hit cache) — keep upstream MVT bytes around so style edits
    // re-render without re-downloading.
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

    let snap = s.style.read().await;
    let png = render_png(&snap, mvt.as_deref(), tile, &s.brushes)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "image/png")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(png))
        .unwrap())
}

/// Return raw decompressed MVT bytes for `(z, x, y)`. Used by the WASM demo,
/// which does its own decoding + rendering client-side.
async fn get_mvt(
    State(s): State<AppState>,
    Path((z, x, y)): Path<(u8, u32, u32)>,
) -> Result<Response, (StatusCode, String)> {
    let tile = TileId::new(z, x, y);
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

fn render_png(
    snap: &StyleSnapshot,
    mvt_bytes: Option<&[u8]>,
    tile: TileId,
    brushes: &std::collections::HashMap<String, Brush>,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let mut canvas = canvas_from_style(&snap.parsed);
    if let Some(bytes) = mvt_bytes {
        let decoded = mvt::decode(bytes)?;
        let resolver = |name: &str| -> Option<&Brush> {
            let key = name.strip_prefix('@').unwrap_or(name);
            brushes.get(key)
        };
        render_style(&mut canvas, &snap.parsed, &decoded, tile, &resolver)?;
    }
    Ok(paint::encode_png(&canvas)?)
}
