//! Render-time intermediate cache, keyed by a content-derived hash.
//!
//! M2 uses a simple `HashMap` with no eviction. Swap to LRU once the
//! render loop is exercised at scale.

use std::collections::HashMap;
use std::sync::Mutex;

use xxhash_rust::xxh3::Xxh3;

use crate::eval::{CanvasInfo, TileId};
use crate::value::PortValue;

/// 128-bit content hash. Wide enough that collisions are not a concern
/// for our scale; narrow enough to fit four words.
pub type Hash128 = u128;

/// Compose a cache key for one node evaluation.
///
/// The key folds together:
/// - the canvas (tile_size + pad), so cached buffers always match shape
/// - the tile id (or omitted for world-anchored nodes)
/// - the node's own param hash
/// - each input's cache hash (Merkle-style chain)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey(pub Hash128);

impl CacheKey {
    pub fn build(
        canvas: CanvasInfo,
        tile: Option<TileId>,
        params_hash: Hash128,
        inputs: &[Hash128],
    ) -> Self {
        let mut h = Xxh3::new();
        h.update(&canvas.tile_size.to_le_bytes());
        h.update(&canvas.pad.to_le_bytes());
        if let Some(t) = tile {
            h.update(&[t.z]);
            h.update(&t.x.to_le_bytes());
            h.update(&t.y.to_le_bytes());
        }
        h.update(&params_hash.to_le_bytes());
        for i in inputs {
            h.update(&i.to_le_bytes());
        }
        CacheKey(h.digest128())
    }
}

/// Shared cache of evaluated `PortValue`s. Cloning a `PortValue` is
/// cheap (it's Arc-backed for the heavy variants) so cache reuse adds
/// near-zero overhead.
pub struct Cache {
    inner: Mutex<HashMap<CacheKey, PortValue>>,
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

impl Cache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub fn get(&self, key: CacheKey) -> Option<PortValue> {
        self.inner.lock().unwrap().get(&key).cloned()
    }

    pub fn insert(&self, key: CacheKey, value: PortValue) {
        self.inner.lock().unwrap().insert(key, value);
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().is_empty()
    }

    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }
}
