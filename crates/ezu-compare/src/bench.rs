//! Standalone timing mode: capture each node's evaluation time from the
//! `ezu_graph::eval` tracing stream and summarise it per op and per node.
//!
//! The graph evaluator emits a `debug` event per node with `node`, `op` and
//! (on a cache miss) `elapsed_us`. A tracing `Layer` collects those events
//! into a shared buffer, which the bench loop drains after each render — so
//! we get a per-node breakdown without changing any of ezu's public APIs.

use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// One node evaluation captured from the eval stream.
#[derive(Clone)]
pub struct EvalRecord {
    pub node: String,
    pub op: String,
    pub elapsed_us: u128,
}

#[derive(Default)]
struct Shared {
    records: Vec<EvalRecord>,
}

/// Handle to the collected records, shared with the installed tracing layer.
#[derive(Clone)]
pub struct EvalCollector {
    inner: Arc<Mutex<Shared>>,
}

impl EvalCollector {
    /// Install a global subscriber that captures `ezu_graph::eval` events and
    /// return a handle to drain them. Call once per process.
    pub fn install() -> EvalCollector {
        use tracing_subscriber::prelude::*;
        let inner = Arc::new(Mutex::new(Shared::default()));
        let layer = EvalLayer {
            inner: inner.clone(),
        };
        tracing_subscriber::registry().with(layer).init();
        EvalCollector { inner }
    }

    /// Drop any records buffered so far (before timing a fresh render).
    pub fn clear(&self) {
        self.inner.lock().unwrap().records.clear();
    }

    /// Take the records captured since the last `clear`/`take`.
    pub fn take(&self) -> Vec<EvalRecord> {
        std::mem::take(&mut self.inner.lock().unwrap().records)
    }
}

struct EvalLayer {
    inner: Arc<Mutex<Shared>>,
}

impl<S: Subscriber> Layer<S> for EvalLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != "ezu_graph::eval" {
            return;
        }
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        // Only cache-miss events carry `elapsed_us`; hits contribute no time.
        if let Some(elapsed_us) = visitor.elapsed_us {
            self.inner.lock().unwrap().records.push(EvalRecord {
                node: visitor.node.unwrap_or_default(),
                op: visitor.op.unwrap_or_default(),
                elapsed_us,
            });
        }
    }
}

#[derive(Default)]
struct FieldVisitor {
    node: Option<String>,
    op: Option<String>,
    elapsed_us: Option<u128>,
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "node" => self.node = Some(value.to_string()),
            "op" => self.op = Some(value.to_string()),
            _ => {}
        }
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        if field.name() == "elapsed_us" {
            self.elapsed_us = Some(value);
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "elapsed_us" {
            self.elapsed_us = Some(value as u128);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // `elapsed_us` may arrive here if the recorder forwards u128 as debug.
        if field.name() == "elapsed_us" && self.elapsed_us.is_none() {
            if let Ok(v) = format!("{value:?}").trim().parse::<u128>() {
                self.elapsed_us = Some(v);
            }
        }
    }
}

/// Sum of every node's eval time, in microseconds.
pub fn eval_total_us(records: &[EvalRecord]) -> u128 {
    records.iter().map(|r| r.elapsed_us).sum()
}

/// Per-op aggregate `(op, count, total_us)`, sorted by total time descending.
pub fn op_table(records: &[EvalRecord]) -> Vec<(String, usize, u128)> {
    use std::collections::HashMap;
    let mut by_op: HashMap<&str, (usize, u128)> = HashMap::new();
    for r in records {
        let e = by_op.entry(r.op.as_str()).or_default();
        e.0 += 1;
        e.1 += r.elapsed_us;
    }
    let mut rows: Vec<(String, usize, u128)> = by_op
        .into_iter()
        .map(|(op, (count, total))| (op.to_string(), count, total))
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.2));
    rows
}

fn us_to_ms(us: u128) -> f64 {
    us as f64 / 1000.0
}

/// Print the per-op breakdown table (op / count / total ms / avg / share%).
pub fn print_op_table(records: &[EvalRecord]) {
    let total_us = eval_total_us(records).max(1);
    println!(
        "{:<18} {:>6} {:>10} {:>9} {:>8}",
        "op", "count", "total ms", "avg ms", "share%"
    );
    for (op, count, op_us) in op_table(records) {
        println!(
            "{:<18} {:>6} {:>10.2} {:>9.3} {:>7.1}%",
            op,
            count,
            us_to_ms(op_us),
            us_to_ms(op_us) / count as f64,
            op_us as f64 / total_us as f64 * 100.0,
        );
    }
}

/// Print the slowest `top` nodes (ms / op / node id).
pub fn print_slow_nodes(records: &[EvalRecord], top: usize) {
    let mut sorted: Vec<&EvalRecord> = records.iter().collect();
    sorted.sort_by_key(|r| std::cmp::Reverse(r.elapsed_us));
    println!("{:>10} {:<18} node", "ms", "op");
    for r in sorted.into_iter().take(top) {
        println!("{:>10.3} {:<18} {}", us_to_ms(r.elapsed_us), r.op, r.node);
    }
}
