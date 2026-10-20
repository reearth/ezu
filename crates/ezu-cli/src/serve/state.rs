//! Shared state for the `ezu serve` live editor + tile server.

use std::path::PathBuf;
use std::sync::Arc;

use ezu::graph::build_graph;
use ezu::paint::nodes::default_registry;
use ezu::renderer::Renderer;
use ezu::style::Document;
use serde::Serialize;
use tokio::sync::{broadcast, RwLock};

use crate::render::{Open, TileRenderer};
use crate::source::{FeatureSources, TileByteCache};

/// State held by every request handler.
#[derive(Clone)]
pub struct AppState {
    /// Every mvt/pmtiles source the style declares, each bound under its
    /// own name, as in the one-shot CLI path.
    pub feature_sources: Arc<FeatureSources>,
    pub style: Arc<RwLock<StyleSnapshot>>,
    /// What every style version is opened with.
    pub sources: Arc<Sources>,
    /// Cached JSON Schema derived from the node registry. Built once at
    /// startup since registry contents don't change at runtime.
    pub schema: Arc<serde_json::Value>,
    /// Broadcast channel for style-reload events. The local-file
    /// watcher task publishes to this; `/style/events` (SSE)
    /// subscribers in the editor receive them.
    pub events: broadcast::Sender<StyleReload>,
}

/// What every style version is opened with: where its files are, its vector
/// sources, and where fetched tiles are kept.
pub struct Sources {
    /// Base directory used to resolve relative asset `src` paths.
    pub assets_dir: PathBuf,
    pub feature_sources: Arc<FeatureSources>,
    /// Maximum number of parent-zoom fallbacks attempted when the
    /// requested tile is missing from the source. `0` disables overzoom.
    pub overzoom_levels: u8,
    /// Remote source tiles, fetched once and kept. Local ones are not
    /// kept: they are read afresh for every request, so an edited tile
    /// shows up on the next one, and only decoded again when its bytes
    /// changed.
    pub byte_cache: Arc<TileByteCache>,
}

/// Payload emitted when the watcher reloads the style from disk.
#[derive(Debug, Clone, Serialize)]
pub struct StyleReload {
    pub version: u64,
    pub text: String,
    /// Unix epoch ms of the on-disk file mtime that triggered this
    /// reload. The editor uses it to render "auto-reloaded HH:MM:SS".
    pub mtime_ms: i64,
}

/// One parsed + built style version. PUT /style atomically swaps the
/// whole snapshot; the renderer, with its intermediate cache, assets and
/// decoded tiles, lives inside, so edits don't poison the next render and
/// any URL-fetched assets get refreshed alongside the document.
pub struct StyleSnapshot {
    pub doc: Document,
    /// Canvas margin to render this style with — the document's `pad`
    /// raised to whatever its filters actually reach (see
    /// `crate::canvas_pad`).
    pub pad: u32,
    pub tiles: Arc<TileRenderer>,
    pub text: String,
    pub version: u64,
}

impl StyleSnapshot {
    /// Parse the document, build the graph, and open a renderer over it:
    /// its assets read against the assets directory, its `dem` and
    /// `raster` sources opened, its remote GeoJSON loaded.
    pub async fn build(
        text: String,
        version: u64,
        sources: &Sources,
    ) -> Result<Self, BuildSnapshotError> {
        let doc = Document::from_json(&text).map_err(BuildSnapshotError::Parse)?;
        // The document parsed, so all the renderer can refuse is the graph.
        let renderer =
            Renderer::new(&text).map_err(|e| BuildSnapshotError::Graph(e.to_string()))?;
        let pad = crate::canvas_pad(renderer.graph(), &doc);
        let tiles = TileRenderer::open(Open {
            renderer,
            doc: &doc,
            pad,
            assets_dir: &sources.assets_dir,
            vector: Arc::clone(&sources.feature_sources),
            overzoom_levels: sources.overzoom_levels,
            params: Vec::new(),
            byte_cache: Some((&sources.byte_cache, true)),
            decoded_budget: crate::DECODED_CACHE_BUDGET,
            cache_budget: None,
        })
        .await
        .map_err(|e| BuildSnapshotError::Assets(e.to_string()))?;
        Ok(Self {
            doc,
            pad,
            tiles: Arc::new(tiles),
            text,
            version,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BuildSnapshotError {
    #[error("parse: {0}")]
    Parse(#[from] ezu::style::StyleError),
    #[error("build graph: {0}")]
    Graph(String),
    #[error("prefetch assets: {0}")]
    Assets(String),
}

/// Lighter twin of [`StyleSnapshot::build`] that runs the parse +
/// graph-build pipeline without prefetching any URL assets. Suitable
/// for live-editor "as you type" validation where round-tripping
/// every keystroke through HTTP fetches would be lethal.
pub fn validate_text(text: &str) -> Result<(), BuildSnapshotError> {
    let doc = Document::from_json(text).map_err(BuildSnapshotError::Parse)?;
    let registry = default_registry();
    build_graph(&doc, &registry).map_err(|e| BuildSnapshotError::Graph(e.to_string()))?;
    Ok(())
}

impl AppState {
    pub fn new(sources: Sources, snapshot: StyleSnapshot) -> Self {
        let schema = default_registry().document_schema();
        // Capacity 8 is enough: events are rare (file saves) and
        // subscribers (open editor tabs) typically count 0–2.
        let (events, _) = broadcast::channel(8);
        Self {
            feature_sources: Arc::clone(&sources.feature_sources),
            style: Arc::new(RwLock::new(snapshot)),
            sources: Arc::new(sources),
            schema: Arc::new(schema),
            events,
        }
    }
}
