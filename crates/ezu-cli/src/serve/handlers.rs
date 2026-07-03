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
use ezu::paint::host::{
    bind_dem_sources, bind_raster_sources, raster_to_png, raster_to_webp,
    requested_neighbor_offsets, BrushBankLoader, DemFetchError, DemSourceRegistry,
    RasterFetchError, RasterSourceRegistry, TileLoader,
};
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
        .route("/style/params", get(get_params_schema))
        .route("/style/attribution", get(get_attribution))
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
        .expect("response builder with valid headers + body never fails")
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
    for a in snap
        .dem_sources
        .resolve_metadata()
        .await
        .unwrap_or_default()
    {
        push(a);
    }
    for a in snap
        .raster_sources
        .resolve_metadata()
        .await
        .unwrap_or_default()
    {
        push(a);
    }
    // The MVT/PMTiles feature source: inherit upstream metadata only
    // when the document's own entry doesn't declare attribution.
    let mvt_declares = snap.doc.sources.values().any(|d| {
        matches!(
            d,
            ezu::style::SourceDecl::Mvt(_) | ezu::style::SourceDecl::Pmtiles(_)
        ) && d.attribution().is_some()
    });
    if !mvt_declares {
        if let Some(src) = s.source.as_ref() {
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

    let fetched = fetch_mvt(&s, tile).await?;

    // Take only what we need from the snapshot to keep the lock window
    // short. Query-string parameter overrides are validated against
    // the document's `params` declarations while we hold the lock.
    let (graph, cache, assets, dem_sources, raster_sources, geojson_inline, tile_size, pad, params) = {
        let snap = s.style.read().await;
        let mut params = ParamValues::new();
        for (name, raw) in &q {
            // `v` is reserved as the editor's cache-busting version key.
            if name == "v" {
                continue;
            }
            let v = ezu::graph::parse_param_value(&snap.doc.params, name, raw)
                .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
            params.set(name.clone(), v);
        }
        // Inline GeoJSON sources are carried in the doc; project them per
        // tile in the render task. Remote-URL geojson is not fetched here.
        let geojson_inline: Vec<(String, serde_json::Value)> = snap
            .doc
            .sources
            .iter()
            .filter_map(|(name, decl)| match decl {
                ezu::style::SourceDecl::GeoJson(g) => match &g.data {
                    Some(d) if d.is_object() || d.is_array() => Some((name.clone(), d.clone())),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        (
            Arc::clone(&snap.graph),
            Arc::clone(&snap.cache),
            Arc::clone(&snap.assets),
            Arc::clone(&snap.dem_sources),
            Arc::clone(&snap.raster_sources),
            geojson_inline,
            snap.doc.tile_size,
            snap.doc.pad,
            params,
        )
    };

    // Cross-tile placement (label collision): fetch only the neighbour
    // tiles the graph asks for via `@dx,dy` binding names — never the
    // whole 3×3 window unconditionally. Reuses the MVT cache per tile.
    let mut neighbor_mvt: Vec<((i32, i32), (bytes::Bytes, CoreTileId))> = Vec::new();
    if let Some(name) = s.source_name.as_deref() {
        let offsets = requested_neighbor_offsets(&graph.asset_inputs(), name);
        let world = 1i64 << tile.z;
        for (dx, dy) in offsets {
            let ny = tile.y as i64 + dy as i64;
            if ny < 0 || ny >= world {
                continue;
            }
            let nx = (tile.x as i64 + dx as i64).rem_euclid(world) as u32;
            if let Some(hit) = fetch_mvt(&s, CoreTileId::new(tile.z, nx, ny as u32)).await? {
                neighbor_mvt.push(((dx, dy), hit));
            }
        }
    }

    let canvas = CanvasInfo { tile_size, pad };
    let tile_id = TileId {
        z: tile.z,
        x: tile.x,
        y: tile.y,
    };
    let dem_bindings = fetch_dem_bindings(&dem_sources, tile_id, canvas)
        .await
        .map_err(dem_fetch_status)?;
    let raster_bindings = fetch_raster_bindings(&raster_sources, tile_id, canvas)
        .await
        .map_err(raster_fetch_status)?;

    let source_name = s.source_name.as_ref().map(Arc::clone);
    let bytes = tokio::task::spawn_blocking({
        move || {
            render_tile(
                &graph,
                &cache,
                &assets,
                fetched,
                neighbor_mvt,
                source_name.as_deref(),
                dem_bindings,
                raster_bindings,
                geojson_inline,
                tile,
                tile_size,
                pad,
                format,
                &params,
            )
        }
    })
    .await
    .map_err(|e| {
        tracing::error!("tile {z}/{x}/{} render task panicked: {e}", tile.y);
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?
    .map_err(|e| {
        tracing::error!("tile {z}/{x}/{}: {e}", tile.y);
        (StatusCode::INTERNAL_SERVER_ERROR, e)
    })?;

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, format.content_type())
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(bytes))
        .expect("response builder with valid headers + body never fails"))
}

/// Return raw decompressed MVT bytes for `(z, x, y)`. Used by the WASM demo,
/// which does its own decoding + rendering client-side.
async fn get_mvt(
    State(s): State<AppState>,
    Path((z, x, y)): Path<(u8, u32, u32)>,
) -> Result<Response, (StatusCode, String)> {
    let tile = CoreTileId::new(z, x, y);
    // This endpoint serves native MVT bytes that callers expect to
    // decode at the requested tile's coordinate frame, so a parent
    // fallback (different coords) would be wrong. Treat overzoom hits
    // as misses here.
    let Some((bytes, src)) = fetch_mvt(&s, tile).await? else {
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
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let tile = CoreTileId::new(z, x, y);
    // Layer names + geometry kinds don't change under overzoom, so it's
    // fine if the bytes came from an ancestor — no clipping needed.
    let Some((bytes, _src)) = fetch_mvt(&s, tile).await? else {
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

/// Fetch the MVT for `tile`, walking up to `overzoom_levels` parents on
/// misses. The returned `CoreTileId` identifies which tile the bytes
/// actually came from — equal to `tile` on a direct hit, an ancestor
/// on overzoom. Callers that intend to render must run those bytes
/// through [`ezu_features::mvt::clip_to_descendant`] when the IDs
/// differ; callers that need the raw native bytes (the `/mvt`
/// endpoint) should treat a non-matching ID as a miss.
///
/// Each level is cached independently, so two sibling tiles requesting
/// the same parent share the fetch.
async fn fetch_mvt(
    s: &AppState,
    tile: CoreTileId,
) -> Result<Option<(bytes::Bytes, CoreTileId)>, (StatusCode, String)> {
    let Some(source) = s.source.as_ref() else {
        return Ok(None);
    };
    let mut current = tile;
    for _ in 0..=s.overzoom_levels {
        if let Some(b) = s.mvt_cache.get(&current).map(|r| r.clone()) {
            return Ok(Some((b, current)));
        }
        match source.fetch(current).await {
            Ok(Some(b)) => {
                s.mvt_cache.insert(current, b.clone());
                return Ok(Some((b, current)));
            }
            Ok(None) => {}
            Err(e) => return Err((StatusCode::BAD_GATEWAY, e.to_string())),
        }
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent;
    }
    Ok(None)
}

/// Pre-fetch the DEM mosaic for every source in the snapshot so the
/// blocking render path receives ready-to-bind [`ScalarField`]s
/// without juggling async fetches.
async fn fetch_dem_bindings(
    registry: &DemSourceRegistry,
    tile: TileId,
    canvas: CanvasInfo,
) -> Result<Vec<(String, ezu::graph::ScalarField)>, DemFetchError> {
    if registry.is_empty() {
        return Ok(Vec::new());
    }
    let base = BrushBankLoader::empty();
    let mut tmp = TileLoader::new(&base, tile);
    bind_dem_sources(&mut tmp, registry, tile, canvas).await?;
    let mut out = Vec::new();
    for name in registry.names() {
        if let Ok(ezu::graph::Asset::ScalarField(field)) = ezu::graph::AssetLoader::load(&tmp, name)
        {
            out.push((name.to_string(), (*field).clone()));
        }
    }
    Ok(out)
}

/// Pre-fetch + stitch every raster source's mosaic for `tile`.
async fn fetch_raster_bindings(
    registry: &RasterSourceRegistry,
    tile: TileId,
    canvas: CanvasInfo,
) -> Result<Vec<(String, ezu::graph::RasterBuf)>, RasterFetchError> {
    if registry.is_empty() {
        return Ok(Vec::new());
    }
    let base = BrushBankLoader::empty();
    let mut tmp = TileLoader::new(&base, tile);
    bind_raster_sources(&mut tmp, registry, tile, canvas).await?;
    let mut out = Vec::new();
    for name in registry.names() {
        if let Ok(ezu::graph::Asset::Image(buf)) = ezu::graph::AssetLoader::load(&tmp, name) {
            out.push((name.to_string(), (*buf).clone()));
        }
    }
    Ok(out)
}

/// `on-missing: error` misses surface as 404 for the rendered tile;
/// everything else is an upstream failure.
fn dem_fetch_status(e: DemFetchError) -> (StatusCode, String) {
    match &e {
        DemFetchError::Missing { .. } => (StatusCode::NOT_FOUND, e.to_string()),
        _ => (StatusCode::BAD_GATEWAY, e.to_string()),
    }
}

fn raster_fetch_status(e: RasterFetchError) -> (StatusCode, String) {
    match &e {
        RasterFetchError::Missing { .. } => (StatusCode::NOT_FOUND, e.to_string()),
        _ => (StatusCode::BAD_GATEWAY, e.to_string()),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_tile(
    graph: &ezu::graph::Graph,
    cache: &ezu::graph::Cache,
    assets: &BrushBankLoader,
    fetched_mvt: Option<(bytes::Bytes, CoreTileId)>,
    neighbor_mvt: Vec<((i32, i32), (bytes::Bytes, CoreTileId))>,
    source_name: Option<&str>,
    dem_bindings: Vec<(String, ezu::graph::ScalarField)>,
    raster_bindings: Vec<(String, ezu::graph::RasterBuf)>,
    geojson_inline: Vec<(String, serde_json::Value)>,
    tile: CoreTileId,
    tile_size: u32,
    pad: u32,
    format: TileFormat,
    params: &ParamValues,
) -> Result<Vec<u8>, String> {
    let tile_id = TileId {
        z: tile.z,
        x: tile.x,
        y: tile.y,
    };
    let mut tile_loader = TileLoader::new(assets, tile_id);
    if let (Some((bytes, src_tile)), Some(src_name)) = (fetched_mvt, source_name) {
        let mut decoded = mvt::decode(&bytes).map_err(|e| format!("mvt decode: {e}"))?;
        if src_tile != tile {
            tracing::debug!(
                "overzoom clip {}/{}/{} ← {}/{}/{}",
                tile.z,
                tile.x,
                tile.y,
                src_tile.z,
                src_tile.x,
                src_tile.y
            );
            decoded = mvt::clip_to_descendant(&decoded, src_tile, tile)
                .map_err(|e| format!("overzoom clip: {e}"))?;
        }
        tile_loader.bind_mvt(src_name, decoded);
        for ((dx, dy), (bytes, src_tile)) in neighbor_mvt {
            let ntile = CoreTileId::new(
                tile.z,
                (tile.x as i64 + dx as i64).rem_euclid(1i64 << tile.z) as u32,
                (tile.y as i64 + dy as i64) as u32,
            );
            let mut decoded = match mvt::decode(&bytes) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("neighbour mvt decode {dx},{dy}: {e}");
                    continue;
                }
            };
            if src_tile != ntile {
                match mvt::clip_to_descendant(&decoded, src_tile, ntile) {
                    Ok(d) => decoded = d,
                    Err(e) => {
                        tracing::warn!("neighbour overzoom clip {dx},{dy}: {e}");
                        continue;
                    }
                }
            }
            tile_loader.bind_mvt_neighbor(src_name, dx, dy, decoded);
        }
    }
    for (name, field) in dem_bindings {
        tile_loader.bind_scalar_field(name, field);
    }
    for (name, buf) in raster_bindings {
        tile_loader.bind_raster(name, buf);
    }
    // Project inline GeoJSON (WGS84) into this tile's frame and bind it as a
    // single feature layer under `<source>.<source>`. When the graph asks
    // for neighbour features (cross-tile label collision), project into
    // those neighbour tiles too and bind them under `@dx,dy` names.
    let requested = graph.asset_inputs();
    for (name, data) in geojson_inline {
        let project = |z: u8, tx: u32, ty: u32| {
            ezu::features::geojson::decode_projected(&data, z, tx, ty, 4096).map(|features| {
                ezu::features::FeatureLayer {
                    name: name.clone(),
                    extent: 4096,
                    features,
                }
            })
        };
        match project(tile.z, tile.x, tile.y) {
            Ok(layer) => {
                tile_loader.bind_features(format!("{name}.{name}"), layer);
            }
            Err(e) => tracing::warn!("geojson source `{name}`: {e}"),
        }
        let world = 1i64 << tile.z;
        for (dx, dy) in requested_neighbor_offsets(&requested, &name) {
            let ny = tile.y as i64 + dy as i64;
            if ny < 0 || ny >= world {
                continue;
            }
            let nx = (tile.x as i64 + dx as i64).rem_euclid(world) as u32;
            match project(tile.z, nx, ny as u32) {
                Ok(layer) => {
                    let base = format!("{name}.{name}");
                    tile_loader.bind_features(ezu::graph::neighbor_binding(&base, dx, dy), layer);
                }
                Err(e) => tracing::warn!("geojson neighbour `{name}` {dx},{dy}: {e}"),
            }
        }
    }
    let ev = Evaluator::new(graph, cache, &tile_loader);
    let out = ev
        .render(
            tile_id,
            CanvasInfo { tile_size, pad },
            params,
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
