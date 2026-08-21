//! Render-time intermediate cache, keyed by a content-derived hash.
//!
//! A bounded LRU keeps long editor sessions from growing without limit.
//! Two limits apply together: an entry count (default 4096) and a budget
//! on the pixel bytes the retained values hold (default
//! [`DEFAULT_BYTE_BUDGET`]). The byte budget is the one that matters for
//! memory: a style with dozens of layers produces dozens of full padded
//! rasters per tile, and counting entries alone would let a single render
//! pin all of them. Tune either via [`Cache::with_limits`].
//!
//! Evicting an entry mid-render is safe: the evaluator holds the values
//! it still needs itself, so the cache only ever decides whether a *later*
//! render gets to skip work.

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
/// - the canvas (both tile axes + pad), so cached buffers always match
///   shape — a buffer cached for one shape must never be handed to a
///   render of another
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
        h.update(&canvas.tile_w.to_le_bytes());
        h.update(&canvas.tile_h.to_le_bytes());
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

/// Default ceiling on pixel bytes retained by cached values: 8 MB, or
/// about seven padded 512 px rasters.
///
/// Values that carry no pixels (features, labels, scalars) are not
/// charged against it, so the budget only governs how many *rasters* a
/// finished render leaves behind for the next one to reuse. A handful
/// covers the hot intermediates an editor session re-renders against,
/// while keeping a memory-constrained host (a 128 MB Workers isolate,
/// say) from paying for a whole style's worth of layers it will never
/// ask for again — on a 68-layer basemap that difference is ~70 MB.
pub const DEFAULT_BYTE_BUDGET: usize = 8 * 1024 * 1024;

/// Shared cache of evaluated `PortValue`s. Cloning a `PortValue` is
/// cheap (Arc-backed for the heavy variants) so cache reuse adds
/// near-zero overhead.
pub struct Cache {
    inner: Mutex<Inner>,
    byte_budget: usize,
}

/// LRU plus the running total of the pixel bytes its entries hold. Both
/// live under one lock so the total can never drift from the contents.
struct Inner {
    lru: LruCache<CacheKey, PortValue>,
    bytes: usize,
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

impl Cache {
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_CAPACITY, DEFAULT_BYTE_BUDGET)
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self::with_limits(cap, DEFAULT_BYTE_BUDGET)
    }

    /// Cache holding at most `cap` entries and at most `byte_budget`
    /// pixel bytes; whichever binds first evicts. A budget of `0`
    /// effectively disables retention of pixel-carrying values, which is
    /// what a one-tile-per-instance host wants.
    pub fn with_limits(cap: usize, byte_budget: usize) -> Self {
        // `cap.max(1)` guarantees the value is non-zero.
        let cap = NonZeroUsize::new(cap.max(1)).expect("cap.max(1) is non-zero");
        Self {
            inner: Mutex::new(Inner {
                lru: LruCache::new(cap),
                bytes: 0,
            }),
            byte_budget,
        }
    }

    /// Look up a cached value and refresh its LRU position.
    pub fn get(&self, key: CacheKey) -> Option<PortValue> {
        self.lock().lru.get(&key).cloned()
    }

    pub fn insert(&self, key: CacheKey, value: PortValue) {
        let bytes = value.approx_bytes();
        let mut inner = self.lock();
        if let Some(old) = inner.lru.put(key, value) {
            inner.bytes = inner.bytes.saturating_sub(old.approx_bytes());
        }
        inner.bytes += bytes;
        // Evict oldest-first until back under budget. The entry just
        // inserted is exempt — evicting it would make `insert` a no-op
        // whenever one value alone exceeds the budget, and callers expect
        // an immediate re-lookup of what they just stored to hit.
        while inner.bytes > self.byte_budget && inner.lru.peek_lru().is_some_and(|(k, _)| *k != key)
        {
            let Some((_, evicted)) = inner.lru.pop_lru() else {
                break;
            };
            inner.bytes = inner.bytes.saturating_sub(evicted.approx_bytes());
        }
    }

    pub fn len(&self) -> usize {
        self.lock().lru.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().lru.is_empty()
    }

    pub fn clear(&self) {
        let mut inner = self.lock();
        inner.lru.clear();
        inner.bytes = 0;
    }

    /// Configured maximum entry count.
    pub fn capacity(&self) -> usize {
        self.lock().lru.cap().get()
    }

    /// Configured ceiling on retained pixel bytes.
    pub fn byte_budget(&self) -> usize {
        self.byte_budget
    }

    /// Pixel bytes currently retained.
    pub fn bytes(&self) -> usize {
        self.lock().bytes
    }

    /// Acquire the inner mutex. Recovers from poisoning by taking the
    /// guard anyway — the cache holds no invariant that a panic mid-op
    /// could break (it's just an LRU of `Arc`s).
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}
