//! Process-wide cache of shaped, laid-out [`TextBlock`]s.
//!
//! A [`TextBlock`] is a pure function of its section text, font stack,
//! `font-scale` / `vertical-align`, size, and [`LayoutParams`](super::LayoutParams)
//! — it carries no tile-relative state — so the same label string laid out
//! against the same stack yields a bit-identical block on every tile. Panning
//! or zooming a basemap re-lays out the same road / place names on adjacent
//! tiles; caching the block across the whole process turns that repeated
//! shaping (the dominant cost of a text node) into a map lookup.
//!
//! The caller owns the key: it hashes every input that reaches
//! [`layout_sections`](super::layout_sections) — the fonts by their stable
//! content hash, plus the section specs, size, and layout params — so two
//! blocks share an entry only when they lay out identically. Values are
//! shared behind an `Arc` and never mutated, so pure-LRU eviction can never
//! change what a later lookup builds.

use std::num::NonZeroUsize;
use std::sync::{Arc, LazyLock, Mutex};

use lru::LruCache;

use super::layout::TextBlock;

/// Entry cap for the shared layout cache. Each entry is one `TextBlock` — a
/// `Vec` of positioned glyphs (~16 B each) plus a bounding box — so a label
/// of a dozen glyphs is a few hundred bytes; 10 000 distinct labels is on the
/// order of single-digit MB. That holds the full set of distinct labels a
/// wide multi-tile pan/zoom session touches (a basemap draws thousands of
/// distinct road / place names across a region) while bounding memory.
const LAYOUT_CAP_ENTRIES: usize = 10_000;

/// The process-wide shaped-layout cache. `LazyLock` so it initializes on
/// first use; a plain `Mutex` (not sharded) since the layout build happens
/// outside the lock — the lock only guards the map lookup / insert.
static LAYOUT: LazyLock<Mutex<LruCache<u64, Arc<TextBlock>>>> = LazyLock::new(|| {
    Mutex::new(LruCache::new(
        NonZeroUsize::new(LAYOUT_CAP_ENTRIES).expect("cap is non-zero"),
    ))
});

/// The cached [`TextBlock`] for `key`, building it with `build` and inserting
/// it on the first process-wide use. Returns the block plus whether it was a
/// cache hit (for hit-rate reporting). `key` must fold every input that
/// affects the layout — the caller is responsible for that (see the module
/// docs).
///
/// The build runs outside the lock, so a race between two threads keying the
/// same label just builds it twice (identically) and the second insert
/// overwrites with an equal value — never a correctness issue.
pub fn get_or_build_layout(key: u64, build: impl FnOnce() -> TextBlock) -> (Arc<TextBlock>, bool) {
    if let Some(hit) = LAYOUT.lock().expect("layout cache poisoned").get(&key) {
        return (hit.clone(), true);
    }
    let block = Arc::new(build());
    LAYOUT
        .lock()
        .expect("layout cache poisoned")
        .put(key, block.clone());
    (block, false)
}
