//! A pull-based sink for the renderer's `tracing` events.
//!
//! The renderer warns about things a host wants to hear — a style whose
//! graph built with warnings, a DEM source whose neighbours were not all
//! bound, so the tile will seam at its border — and none of that is an
//! error, so none of it comes back through a return code.
//!
//! Events are buffered and drained on request rather than written to
//! stdout. A wasip1 instance's stdio is torn down with the instance, so a
//! host that renders and then closes can lose whatever was still in the
//! pipe; and pulling is what lets a host attribute the lines to the tile it
//! was rendering, since it drains right after the call that produced them.
//!
//! The filter is a level, not a filter string. `tracing-subscriber`'s
//! `env-filter` would allow per-target directives, but it carries a regex
//! engine — 366 kB of the module — and this shell offers no way to change
//! the filter at runtime that would justify it. (The npm package keeps
//! `env-filter`: papers configures logging through a directive string, and
//! narrowing that argument would be a change to a shipped surface.)

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::Registry;
use tracing_subscriber::Layer;

/// Lines kept before the oldest are dropped. A host that drains after every
/// render never approaches it; one that never drains is bounded rather than
/// slowly eating the heap it is trying to render in.
const CAPACITY: usize = 4096;

static BUFFER: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

fn buffer() -> &'static Mutex<VecDeque<String>> {
    BUFFER.get_or_init(|| Mutex::new(VecDeque::new()))
}

struct BufferLayer;

impl<S: Subscriber> Layer<S> for BufferLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut visitor = FieldCollector::default();
        event.record(&mut visitor);
        let mut line = format!("{} {}: {}", meta.level(), meta.target(), visitor.message);
        for (k, v) in &visitor.fields {
            line.push_str(&format!(" {k}={v}"));
        }
        let Ok(mut buf) = buffer().lock() else {
            return;
        };
        while buf.len() >= CAPACITY {
            buf.pop_front();
        }
        buf.push_back(line);
    }
}

/// Walk an event's fields, separating the `message` field (special-cased by
/// `tracing`) from the rest.
#[derive(Default)]
struct FieldCollector {
    message: String,
    fields: Vec<(String, String)>,
}

impl FieldCollector {
    fn put(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = value;
        } else {
            self.fields.push((field.name().to_string(), value));
        }
    }
}

impl Visit for FieldCollector {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // Strip the quoting `Debug` adds for strings, so a host reading the
        // line sees plain values.
        let raw = format!("{value:?}");
        let cleaned = match raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            Some(inner) if !inner.contains('"') => inner.to_string(),
            _ => raw,
        };
        self.put(field, cleaned);
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, value.to_string());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, value.to_string());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, value.to_string());
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field, value.to_string());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, value.to_string());
    }
}

/// `0` off, `1` error, `2` warn, `3` info, `4` debug, `5` trace. Anything
/// higher is trace.
fn level_filter(level: u32) -> LevelFilter {
    match level {
        0 => LevelFilter::OFF,
        1 => LevelFilter::ERROR,
        2 => LevelFilter::WARN,
        3 => LevelFilter::INFO,
        4 => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    }
}

static INSTALLED: OnceLock<bool> = OnceLock::new();

/// Install the sink. Idempotent: the level of the first call wins, since
/// `tracing`'s global subscriber can only be set once. Returns 0 on the
/// call that installed it and 0 on every later one too — a host calling
/// this twice has made no mistake worth a code.
pub fn init(level: u32) -> i64 {
    let _ = INSTALLED.get_or_init(|| {
        let subscriber = Registry::default().with(BufferLayer.with_filter(level_filter(level)));
        tracing::subscriber::set_global_default(subscriber).is_ok()
    });
    0
}

/// Take every buffered line, newline-separated, and empty the buffer.
pub fn drain() -> String {
    let Ok(mut buf) = buffer().lock() else {
        return String::new();
    };
    let lines: Vec<String> = buf.drain(..).collect();
    lines.join("\n")
}
