//! Shared state for the `ezu serve` live editor + tile server.

use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;
use ezu::core::TileId;
use ezu::graph::{build_graph, Cache, Graph};
use ezu::paint::host::BrushBankLoader;
use ezu::paint::nodes::default_registry;
use ezu::style::Document;
use tokio::sync::RwLock;

use crate::source::TileSource;

/// State held by every request handler.
#[derive(Clone)]
pub struct AppState {
    pub source: Arc<TileSource>,
    pub style: Arc<RwLock<StyleSnapshot>>,
    /// Base directory used to resolve relative asset `src` paths and
    /// as the fall-through for the per-snapshot loader's disk lookups.
    pub assets_dir: Arc<PathBuf>,
    pub mvt_cache: Arc<DashMap<TileId, Bytes>>,
    /// Cached JSON Schema derived from the node registry. Built once at
    /// startup since registry contents don't change at runtime.
    pub schema: Arc<serde_json::Value>,
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
        Ok(Self {
            doc,
            graph: Arc::new(graph),
            cache: Arc::new(Cache::new()),
            assets: Arc::new(loader),
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
    pub fn new(source: TileSource, snapshot: StyleSnapshot, assets_dir: PathBuf) -> Self {
        let schema = default_registry().document_schema();
        Self {
            source: Arc::new(source),
            style: Arc::new(RwLock::new(snapshot)),
            assets_dir: Arc::new(assets_dir),
            mvt_cache: Arc::new(DashMap::new()),
            schema: Arc::new(schema),
        }
    }
}
