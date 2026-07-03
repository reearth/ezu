//! MapLibre layer filters → ezu.
//!
//! A layer's `filter` is routed by [`layer_filter_expr`]:
//!
//! - **expression-form** filters (per `maplibre_expr::is_expression_filter`)
//!   pass through verbatim as a raw `filter-expr`, which ezu-paint evaluates
//!   via the `maplibre-expr` crate — full fidelity, including `any`,
//!   `has`/`!has`, `<`/`>`, `geometry-type`/`$type`, etc.
//! - **legacy-form** filters (bare property names, `!in`/`!has`/`none`) are
//!   converted to the equivalent expression by
//!   `maplibre_expr::convert_legacy_filter` — the same conversion MapLibre
//!   itself applies before compiling — and emitted as `filter-expr` too. Only
//!   a structurally malformed legacy filter is reported and left unfiltered.

use serde_json::Value;

use crate::maplibre::Report;

/// The raw expression filter to attach to a layer's features node, if any.
///
/// Expression-form filters pass through verbatim; legacy-form filters are
/// converted to expressions (matching MapLibre's own conversion). A malformed
/// legacy filter → `None` plus a warning; the layer is left unfiltered.
pub(crate) fn layer_filter_expr(
    layer: &serde_json::Map<String, Value>,
    report: &mut Report,
    id: &str,
) -> Option<Value> {
    let f = layer.get("filter")?;
    if maplibre_expr::is_expression_filter(f) {
        return Some(f.clone());
    }
    match maplibre_expr::convert_legacy_filter(f) {
        Ok(expr) => Some(expr),
        Err(e) => {
            report.warn(format!(
                "layer `{id}`: malformed legacy filter ({e}) — layer left unfiltered"
            ));
            None
        }
    }
}
