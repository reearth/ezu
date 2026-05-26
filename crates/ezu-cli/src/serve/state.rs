//! Shared state for the `ezu serve` live editor + tile server.

use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;
use ezu::core::TileId;
use ezu::graph::{build_graph, Cache, Graph};
use ezu::paint::host::{build_dem_sources, BrushBankLoader, DemSourceRegistry};
use ezu::paint::nodes::default_registry;
use ezu::style::Document;
use serde::Serialize;
use tokio::sync::{broadcast, RwLock};

use crate::source::TileSource;

/// State held by every request handler.
#[derive(Clone)]
pub struct AppState {
    pub source: Option<Arc<TileSource>>,
    /// Document source name that `source`'s bytes get bound under.
    /// Mirrors the `Prepared::source_name` in the one-shot CLI path.
    pub source_name: Option<Arc<str>>,
    pub style: Arc<RwLock<StyleSnapshot>>,
    /// Base directory used to resolve relative asset `src` paths and
    /// as the fall-through for the per-snapshot loader's disk lookups.
    pub assets_dir: Arc<PathBuf>,
    pub mvt_cache: Arc<DashMap<TileId, Bytes>>,
    /// Maximum number of parent-zoom fallbacks attempted when the
    /// requested tile is missing from the source. `0` disables overzoom.
    pub overzoom_levels: u8,
    /// Cached JSON Schema derived from the node registry. Built once at
    /// startup since registry contents don't change at runtime.
    pub schema: Arc<serde_json::Value>,
    /// Broadcast channel for style-reload events. The local-file
    /// watcher task publishes to this; `/style/events` (SSE)
    /// subscribers in the editor receive them.
    pub events: broadcast::Sender<StyleReload>,
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
/// whole snapshot; the per-style intermediate cache and asset loader
/// both live inside, so edits don't poison the next render and any
/// URL-fetched assets get refreshed alongside the document.
pub struct StyleSnapshot {
    pub doc: Document,
    pub graph: Arc<Graph>,
    pub cache: Arc<Cache>,
    pub assets: Arc<BrushBankLoader>,
    /// One fetcher per `dem` entry in the document's `sources` block.
    /// Rebuilt with the snapshot so a style edit picks up new DEM URLs.
    pub dem_sources: Arc<DemSourceRegistry>,
    pub text: String,
    pub version: u64,
}

impl StyleSnapshot {
    /// Parse the document, build the graph, and pre-resolve every entry
    /// in `doc.assets` (URL or local path) into a fresh `BrushBankLoader`.
    /// Disk lookups for assets that the doc doesn't declare fall
    /// through to `assets_dir`.
    pub async fn build(
        text: String,
        version: u64,
        assets_dir: &std::path::Path,
    ) -> Result<Self, BuildSnapshotError> {
        let doc = Document::from_json(&text).map_err(BuildSnapshotError::Parse)?;
        let registry = default_registry();
        let graph = build_graph(&doc, &registry).map_err(BuildSnapshotError::Graph)?;
        let mut loader = BrushBankLoader::new()
            .with_dir(assets_dir.to_path_buf())
            .with_images_dir(assets_dir.to_path_buf());
        ezu::paint::host::prefetch_doc_assets(&doc, assets_dir, &mut loader)
            .await
            .map_err(BuildSnapshotError::Assets)?;
        let dem_sources = Arc::new(build_dem_sources(&doc));
        Ok(Self {
            doc,
            graph: Arc::new(graph),
            cache: Arc::new(Cache::new()),
            assets: Arc::new(loader),
            dem_sources,
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
    Graph(#[from] ezu::graph::BuildGraphError),
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
    build_graph(&doc, &registry).map_err(BuildSnapshotError::Graph)?;
    Ok(())
}

impl AppState {
    pub fn new(
        source: Option<TileSource>,
        source_name: Option<String>,
        snapshot: StyleSnapshot,
        assets_dir: PathBuf,
        overzoom_levels: u8,
    ) -> Self {
        let schema = default_registry().document_schema();
        // Capacity 8 is enough: events are rare (file saves) and
        // subscribers (open editor tabs) typically count 0–2.
        let (events, _) = broadcast::channel(8);
        Self {
            source: source.map(Arc::new),
            source_name: source_name.map(Arc::from),
            style: Arc::new(RwLock::new(snapshot)),
            assets_dir: Arc::new(assets_dir),
            mvt_cache: Arc::new(DashMap::new()),
            schema: Arc::new(schema),
            events,
            overzoom_levels,
        }
    }
}
