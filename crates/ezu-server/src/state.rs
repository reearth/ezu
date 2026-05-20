use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;
use ezu::core::TileId;
use ezu::paint::Brush;
use ezu::pmtiles::PmTilesArchive;
use ezu::style::Style;
use tokio::sync::RwLock;

/// State held by every request handler.
#[derive(Clone)]
pub struct AppState {
    pub archive: Arc<PmTilesArchive>,
    pub style: Arc<RwLock<StyleSnapshot>>,
    pub brushes: Arc<HashMap<String, Brush>>,
    pub mvt_cache: Arc<DashMap<TileId, Bytes>>,
    pub schema_path: PathBuf,
}

pub struct StyleSnapshot {
    pub parsed: Style,
    pub text: String,
    pub version: u64,
}

impl AppState {
    pub fn new(
        archive: PmTilesArchive,
        parsed: Style,
        text: String,
        brushes: HashMap<String, Brush>,
        schema_path: PathBuf,
    ) -> Self {
        Self {
            archive: Arc::new(archive),
            style: Arc::new(RwLock::new(StyleSnapshot {
                parsed,
                text,
                version: 1,
            })),
            brushes: Arc::new(brushes),
            mvt_cache: Arc::new(DashMap::new()),
            schema_path,
        }
    }
}
