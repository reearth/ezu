use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;
use ezu::core::TileId;
use ezu::graph::{build_graph, Cache, Graph};
use ezu::paint::host::BrushBankLoader;
use ezu::paint::nodes::default_registry;
use crate::pmtiles::PmTilesArchive;
use ezu::style::Document;
use tokio::sync::RwLock;

/// State held by every request handler.
#[derive(Clone)]
pub struct AppState {
    pub archive: Arc<PmTilesArchive>,
    pub style: Arc<RwLock<StyleSnapshot>>,
    pub assets: Arc<BrushBankLoader>,
    pub mvt_cache: Arc<DashMap<TileId, Bytes>>,
    /// Cached JSON Schema derived from the node registry. Built once at
    /// startup since registry contents don't change at runtime.
    pub schema: Arc<serde_json::Value>,
}

/// One parsed + built style version. PUT /style atomically swaps the
/// whole snapshot; the per-style intermediate cache lives inside, so
/// edits don't poison the next render.
pub struct StyleSnapshot {
    pub doc: Document,
    pub graph: Arc<Graph>,
    pub cache: Arc<Cache>,
    pub text: String,
    pub version: u64,
}

impl StyleSnapshot {
    pub fn build(text: String, version: u64) -> Result<Self, BuildSnapshotError> {
        let doc = Document::from_json(&text).map_err(BuildSnapshotError::Parse)?;
        let registry = default_registry();
        let graph = build_graph(&doc, &registry).map_err(BuildSnapshotError::Graph)?;
        Ok(Self {
            doc,
            graph: Arc::new(graph),
            cache: Arc::new(Cache::new()),
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
}

impl AppState {
    pub fn new(
        archive: PmTilesArchive,
        snapshot: StyleSnapshot,
        assets: BrushBankLoader,
    ) -> Self {
        let schema = default_registry().document_schema();
        Self {
            archive: Arc::new(archive),
            style: Arc::new(RwLock::new(snapshot)),
            assets: Arc::new(assets),
            mvt_cache: Arc::new(DashMap::new()),
            schema: Arc::new(schema),
        }
    }
}
