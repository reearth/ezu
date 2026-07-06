//! MapLibre layer filters → ezu.
//!
//! A layer's `filter` is routed by [`layer_filter_expr`] through
//! `maplibre_expr::convert_legacy_filter`, which:
//!
//! - passes **expression-form** filters through verbatim as a raw `filter-expr`
//!   (ezu-paint evaluates them via the `maplibre-expr` crate — full fidelity,
//!   including `any`, `has`/`!has`, `<`/`>`, `geometry-type`/`$type`, etc.);
//! - converts **legacy-form** filters (bare property names, `!in`/`!has`/`none`)
//!   to the equivalent expression — the same conversion MapLibre itself applies
//!   before compiling;
//! - and rewrites the legacy leaves of a **mixed** `all`/`any`/`none` combiner
//!   (one MapLibre would reject) so real-world styles such as the Protomaps
//!   basemap still render, rather than leaving a raw legacy operator for the
//!   evaluator to reject.
//!
//! Only a structurally malformed legacy filter is reported and left unfiltered.

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
    // `convert_legacy_filter` passes genuine expressions through unchanged,
    // converts legacy filters, and rewrites the legacy leaves of a *mixed*
    // `all`/`any`/`none` combiner (which `is_expression_filter` would otherwise
    // classify as an expression and leave a raw `!has` / three-arg `==` for the
    // evaluator to choke on — Protomaps basemap hits exactly this).
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
