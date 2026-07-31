//! Opt-in accounting of the pixel buffers a render keeps alive.
//!
//! Peak RSS tells you a render was expensive; it does not tell you which
//! nodes were holding the bytes when the peak happened. This module adds
//! the missing half: the evaluator reports every intermediate it starts
//! and stops holding, and the tracker keeps a running live total, its
//! high-water mark, and a per-op breakdown.
//!
//! Everything is behind the `EZU_MEM_REPORT` environment variable, so a
//! normal render pays one relaxed atomic load per node. The counters are
//! process-wide and reset at the start of each render, so the report
//! describes one render — read it from a single-render run, not from a
//! server serving tiles concurrently.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static TOTAL: AtomicUsize = AtomicUsize::new(0);

/// Per-op totals: (bytes ever produced, buffers ever produced).
static BY_OP: OnceLock<Mutex<BTreeMap<&'static str, (usize, usize)>>> = OnceLock::new();

fn by_op() -> &'static Mutex<BTreeMap<&'static str, (usize, usize)>> {
    BY_OP.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Whether reporting is on. Read once per process from `EZU_MEM_REPORT`.
pub fn enabled() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| {
            std::env::var("EZU_MEM_REPORT")
                .map(|v| v != "0" && !v.is_empty())
                .unwrap_or(false)
        })
    }
}

/// Record that `bytes` of intermediate pixels became live, produced by `op`.
pub fn acquired(op: &'static str, bytes: usize) {
    if bytes == 0 {
        return;
    }
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
    TOTAL.fetch_add(bytes, Ordering::Relaxed);
    let mut m = by_op().lock().unwrap_or_else(|e| e.into_inner());
    let e = m.entry(op).or_insert((0, 0));
    e.0 += bytes;
    e.1 += 1;
}

/// Record that `bytes` of intermediate pixels were dropped.
pub fn released(bytes: usize) {
    if bytes == 0 {
        return;
    }
    LIVE.fetch_sub(bytes, Ordering::Relaxed);
}

/// Live bytes, high-water bytes, and cumulative bytes produced.
pub fn snapshot() -> (usize, usize, usize) {
    (
        LIVE.load(Ordering::Relaxed),
        PEAK.load(Ordering::Relaxed),
        TOTAL.load(Ordering::Relaxed),
    )
}

pub fn reset() {
    LIVE.store(0, Ordering::Relaxed);
    PEAK.store(0, Ordering::Relaxed);
    TOTAL.store(0, Ordering::Relaxed);
    by_op().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Human-readable breakdown, heaviest op first.
pub fn report() -> String {
    let (live, peak, total) = snapshot();
    let mb = |b: usize| b as f64 / (1024.0 * 1024.0);
    let mut out = format!(
        "intermediate pixel buffers: peak live {:.1} MB, still live {:.1} MB, cumulative {:.1} MB\n",
        mb(peak),
        mb(live),
        mb(total),
    );
    let m = by_op().lock().unwrap_or_else(|e| e.into_inner());
    let mut rows: Vec<(&&str, &(usize, usize))> = m.iter().collect();
    rows.sort_by_key(|(_, v)| std::cmp::Reverse(v.0));
    for (op, (bytes, count)) in rows {
        out.push_str(&format!(
            "  {op:<20} {:>8.1} MB over {count} buffer(s)\n",
            mb(*bytes)
        ));
    }
    out
}
