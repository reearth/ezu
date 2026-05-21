//! Render-time intermediate cache, keyed by a content-derived hash.
//!
//! A bounded LRU keeps long editor sessions from growing without limit.
//! Default capacity is 4096 entries; tune via [`Cache::with_capacity`].
//! At ~1.3 MB per padded raster that ceilings around ~5 GB worst case,
//! but in practice intermediates are mostly small (masks, features) and
//! the cap-by-count is the right knob.

use std::num::NonZeroUsize;
use std::sync::Mutex;

use lru::LruCache;
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

/// Default LRU capacity. Each entry holds an `Arc<PortValue>` so the
/// payload is shared, not duplicated; the cap bounds how many distinct
/// intermediates the evaluator remembers, not raw bytes.
pub const DEFAULT_CAPACITY: usize = 4096;

/// Shared cache of evaluated `PortValue`s. Cloning a `PortValue` is
/// cheap (Arc-backed for the heavy variants) so cache reuse adds
/// near-zero overhead.
pub struct Cache {
    inner: Mutex<LruCache<CacheKey, PortValue>>,
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

impl Cache {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    pub fn with_capacity(cap: usize) -> Self {
        let cap = NonZeroUsize::new(cap.max(1)).unwrap();
        Self {
            inner: Mutex::new(LruCache::new(cap)),
        }
    }

    /// Look up a cached value and refresh its LRU position.
    pub fn get(&self, key: CacheKey) -> Option<PortValue> {
        self.inner.lock().unwrap().get(&key).cloned()
    }

    pub fn insert(&self, key: CacheKey, value: PortValue) {
        self.inner.lock().unwrap().put(key, value);
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

    /// Configured maximum entry count.
    pub fn capacity(&self) -> usize {
        self.inner.lock().unwrap().cap().get()
    }
}
