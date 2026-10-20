//! `raster` layer → an ezu `raster` node bound to the named source.

use serde_json::{Map, Value};

use crate::maplibre::layers::with_zoom_range;
use crate::maplibre::{Report, ZoomRange};

pub(crate) fn convert_raster(
    id: &str,
    layer: &Map<String, Value>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    zoom_range: ZoomRange,
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
        with_zoom_range(
            serde_json::json!({ "op": "raster", "source": src }),
            zoom_range,
        ),
    );
    outputs.push(nid);
}
