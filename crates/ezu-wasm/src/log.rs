//! WASM-friendly log sink: forwards every `tracing` event the renderer
//! produces (notably the per-node `ezu_graph::eval` lines) into a
//! shared buffer that JS can drain on its own cadence, and/or into a
//! JS callback for live tailing.
//!
//! Designed for browser-side debugging panels: install once, attach a
//! callback to mirror events to `console.log` or a UI list, and call
//! `drain` / `drainLines` whenever you want to dump everything since
//! the last drain.
//!
//! Idempotent: re-constructing a `LogSink` returns a handle to the
//! same global state — the underlying `tracing` subscriber is
//! installed only on the first call. The `level` argument is honoured
//! on first install and ignored afterwards (filter changes at runtime
//! would need `tracing-subscriber`'s reload layer; not wired today).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::Registry;
use tracing_subscriber::{EnvFilter, Layer};
use wasm_bindgen::prelude::*;

const DEFAULT_CAPACITY: usize = 4096;

/// Shared between the global Layer and every `LogSink` handle JS holds.
#[derive(Clone)]
struct LogState {
    buffer: Arc<Mutex<VecDeque<LogRecord>>>,
    callback: Arc<Mutex<Option<js_sys::Function>>>,
    capacity: Arc<Mutex<usize>>,
}

impl LogState {
    fn new() -> Self {
        Self {
            buffer: Arc::new(Mutex::new(VecDeque::with_capacity(DEFAULT_CAPACITY))),
            callback: Arc::new(Mutex::new(None)),
            capacity: Arc::new(Mutex::new(DEFAULT_CAPACITY)),
        }
    }

    fn push(&self, record: LogRecord) {
        // Invoke callback first so live consumers see events even if
        // the ring buffer is being aggressively drained elsewhere.
        if let Ok(cb) = self.callback.lock() {
            if let Some(cb) = &*cb {
                let js = record_to_js(&record);
                let _ = cb.call1(&JsValue::NULL, &js);
            }
        }
        if let Ok(mut buf) = self.buffer.lock() {
            let cap = *self.capacity.lock().unwrap_or_else(|e| e.into_inner());
            while buf.len() >= cap {
                buf.pop_front();
            }
            buf.push_back(record);
        }
    }
}

#[derive(Clone, Debug)]
struct LogRecord {
    level: &'static str,
    target: String,
    message: String,
    fields: Vec<(String, String)>,
    timestamp_ms: f64,
}

struct LogLayer {
    state: LogState,
}

impl<S: Subscriber> Layer<S> for LogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let mut visitor = FieldCollector::default();
        event.record(&mut visitor);
        let record = LogRecord {
            level: metadata.level().as_str(),
            target: metadata.target().to_string(),
            message: visitor.message,
            fields: visitor.fields,
            timestamp_ms: js_sys::Date::now(),
        };
        self.state.push(record);
    }
}

/// Walk an event's fields, separating the `message` field (special-cased
/// by `tracing`) from the rest. Everything is stringified via `Debug`
/// so structured numeric fields keep their natural representation.
#[derive(Default)]
struct FieldCollector {
    message: String,
    fields: Vec<(String, String)>,
}

impl FieldCollector {
    fn record(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = value;
        } else {
            self.fields.push((field.name().to_string(), value));
        }
    }
}

impl Visit for FieldCollector {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // Strip the `"..."` quoting `Debug` adds for strings so JS
        // consumers see plain values.
        let raw = format!("{value:?}");
        let cleaned = match raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            Some(inner) if !inner.contains('"') => inner.to_string(),
            _ => raw,
        };
        self.record(field, cleaned);
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.record(field, value.to_string());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record(field, value.to_string());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record(field, value.to_string());
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.record(field, value.to_string());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record(field, value.to_string());
    }
}

static GLOBAL_STATE: OnceLock<LogState> = OnceLock::new();

/// Buffered log sink that captures `tracing` events from the renderer.
///
/// Construct once near the top of your bootstrap, then either set a
/// callback for live forwarding (`onEvent(e => console.log(e))`) or
/// `drainLines()` periodically to flush into a UI panel.
#[wasm_bindgen]
pub struct LogSink {
    state: LogState,
}

#[wasm_bindgen]
impl LogSink {
    /// Install (idempotently) the global tracing subscriber. `level` is
    /// an `EnvFilter` string — e.g. `"info"`, `"debug"`,
    /// `"info,ezu_graph::eval=debug"`. Honoured on first call only.
    #[wasm_bindgen(constructor)]
    pub fn new(level: &str) -> Result<LogSink, JsValue> {
        let state = GLOBAL_STATE
            .get_or_init(|| install(level).unwrap_or_else(|_| LogState::new()))
            .clone();
        Ok(LogSink { state })
    }

    /// Set (or clear with `null` / `undefined`) the per-event JS
    /// callback. The callback receives a structured record object.
    #[wasm_bindgen(js_name = onEvent)]
    pub fn on_event(&self, callback: Option<js_sys::Function>) {
        if let Ok(mut cb) = self.state.callback.lock() {
            *cb = callback;
        }
    }

    /// Drain every buffered record. Returns a JS array of structured
    /// objects `{ level, target, message, fields, timestampMs }`.
    pub fn drain(&self) -> js_sys::Array {
        let arr = js_sys::Array::new();
        let mut buf = match self.state.buffer.lock() {
            Ok(b) => b,
            Err(e) => e.into_inner(),
        };
        for record in buf.drain(..) {
            arr.push(&record_to_js(&record));
        }
        arr
    }

    /// Drain buffered records and return pre-formatted strings — the
    /// same shape the CLI prints. Convenient for `console.log(lines.join('\n'))`.
    #[wasm_bindgen(js_name = drainLines)]
    pub fn drain_lines(&self) -> Vec<String> {
        let mut buf = match self.state.buffer.lock() {
            Ok(b) => b,
            Err(e) => e.into_inner(),
        };
        buf.drain(..).map(|r| format_line(&r)).collect()
    }

    /// Drop all buffered records without returning them.
    pub fn clear(&self) {
        if let Ok(mut buf) = self.state.buffer.lock() {
            buf.clear();
        }
    }

    /// Number of records currently buffered.
    #[wasm_bindgen(getter)]
    pub fn len(&self) -> usize {
        self.state.buffer.lock().map(|b| b.len()).unwrap_or(0)
    }

    /// Resize the ring-buffer cap. Older entries are evicted FIFO once
    /// the buffer reaches the new cap. Default is 4096.
    #[wasm_bindgen(js_name = setCapacity)]
    pub fn set_capacity(&self, cap: usize) {
        if let Ok(mut c) = self.state.capacity.lock() {
            *c = cap.max(1);
        }
        if let Ok(mut buf) = self.state.buffer.lock() {
            while buf.len() > cap {
                buf.pop_front();
            }
        }
    }
}

fn install(level: &str) -> Result<LogState, ()> {
    let filter = EnvFilter::try_new(level).map_err(|_| ())?;
    let state = LogState::new();
    let layer = LogLayer {
        state: state.clone(),
    }
    .with_filter(filter);
    let subscriber = Registry::default().with(layer);
    // If another subscriber is already installed (e.g. a host that
    // wired its own up before us) we silently fall back to a sink
    // that simply never receives events. The user still gets a valid
    // handle so their JS doesn't have to special-case the failure.
    tracing::subscriber::set_global_default(subscriber).map_err(|_| ())?;
    Ok(state)
}

fn record_to_js(r: &LogRecord) -> JsValue {
    let obj = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&obj, &"level".into(), &r.level.into());
    let _ = js_sys::Reflect::set(&obj, &"target".into(), &r.target.as_str().into());
    let _ = js_sys::Reflect::set(&obj, &"message".into(), &r.message.as_str().into());
    let _ = js_sys::Reflect::set(&obj, &"timestampMs".into(), &r.timestamp_ms.into());
    let fields = js_sys::Object::new();
    for (k, v) in &r.fields {
        let _ = js_sys::Reflect::set(&fields, &k.as_str().into(), &v.as_str().into());
    }
    let _ = js_sys::Reflect::set(&obj, &"fields".into(), &fields);
    obj.into()
}

fn format_line(r: &LogRecord) -> String {
    let mut s = format!(
        "{} {} {}: {}",
        r.timestamp_ms as u64, r.level, r.target, r.message
    );
    for (k, v) in &r.fields {
        s.push_str(&format!(" {k}={v}"));
    }
    s
}
