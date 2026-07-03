//! `raster` layer → an ezu `raster` node bound to the named source.

use serde_json::{Map, Value};

use crate::maplibre::Report;

pub(crate) fn convert_raster(
    id: &str,
    layer: &Map<String, Value>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    report: &mut Report,
) {
    // A raster layer references a raster source by name; ezu's `raster`
    // node picks it up by source name. We already emitted the source.
    let Some(src) = layer.get("source").and_then(Value::as_str) else {
        report.warn(format!("layer `{id}`: raster without source — skipped"));
        return;
    };
    let nid = format!("{id}__raster");
    nodes.insert(
        nid.clone(),
        serde_json::json!({ "op": "raster", "source": src }),
    );
    outputs.push(nid);
}
