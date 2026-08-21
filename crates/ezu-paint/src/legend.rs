//! Rendering a legend entry's swatch.
//!
//! A legend entry names the node that draws its symbol
//! ([`ezu_style::LegendEntry`]), not what the symbol looks like, because
//! in this renderer a symbol is rarely a colour. A watercolour fill is
//! brush dabs; a sketched road is a jittered stroke; a dot density layer
//! is a scatter. None of that reduces to a hex value a host could put in
//! a `<div>`.
//!
//! So a swatch is drawn by the renderer, through the same pipeline and
//! the same node the map uses. Two pieces make that possible without
//! any special path through the evaluator:
//!
//! - the entry's node and its ancestors are lifted into a document of
//!   their own, whose output *is* that node ([`Document::subgraph`]), so
//!   an ordinary `build_graph` + `render` draws just the symbol, with no
//!   basemap under it and no unrelated source to fetch
//! - the features the graph asks for are answered with one synthetic
//!   feature carrying the entry's declared properties, so whatever the
//!   node would draw for a real feature of that description is what the
//!   swatch shows
//!
//! The canvas need not be square, which is why [`CanvasInfo`] has two
//! sides: a swatch is as wide and as tall as the legend has room for.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use ezu_features::{Feature, FeatureLayer, Geometry, Polygon, Value as FeatureValue};
use ezu_graph::{
    build_graph, Asset, AssetError, AssetLoader, BuildGraphError, Cache, CanvasInfo, Evaluator,
    NodeRegistry, OpaqueValue, ParamValues, PortValue, RasterBuf, RenderError, TileId,
};
use ezu_style::{Document, LegendEntry, LegendGeometry};
use xxhash_rust::xxh3::Xxh3;

use crate::host::looks_like_asset_src;
use crate::render::SharedLayer;

/// Coordinate extent of the synthetic feature. Matches the MVT
/// convention, so `extent`-sized fields behave as they do on a tile.
const EXTENT: u32 = 4096;

/// Shape and scale to draw a swatch at.
#[derive(Debug, Clone, Copy)]
pub struct SwatchOptions {
    pub width: u32,
    pub height: u32,
    /// Zoom the symbol is shown as it appears at. Every zoom curve in
    /// the style — `interpolate`, `step`, a `zoom` node — reads this, so
    /// a swatch is only true for the zoom it was asked for.
    pub zoom: u8,
    /// Floor for the canvas padding. The graph's own requirement is
    /// taken as well, so a filter never renders against a clamped edge.
    pub pad: u32,
    /// Geometry for entries that do not name one themselves.
    pub geometry: LegendGeometry,
}

impl Default for SwatchOptions {
    fn default() -> Self {
        Self {
            width: 48,
            height: 32,
            zoom: 12,
            pad: 0,
            geometry: LegendGeometry::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SwatchError {
    #[error("legend entry `{label}` names `@{src}`, which is not a node in this style")]
    UnknownNode { label: String, src: String },
    #[error("legend entry `{label}`: {source}")]
    Build {
        label: String,
        // Boxed: these carry a lot, and every caller of `render_swatch`
        // would otherwise pay for it on the happy path too.
        #[source]
        source: Box<BuildGraphError>,
    },
    #[error("legend entry `{label}`: {source}")]
    Render {
        label: String,
        #[source]
        source: Box<RenderError>,
    },
    #[error("legend entry `{label}` names `@{src}`, which produced {got} rather than a raster")]
    NotRaster {
        label: String,
        src: String,
        got: String,
    },
}

/// Draw `entry`'s symbol.
///
/// Returns the **padded** buffer together with the canvas it was drawn
/// on, because cropping and encoding already have owners:
/// [`crop_to_png`](crate::host::crop_to_png) and its siblings take a
/// width, a height and the pad.
///
/// `assets` supplies the document-scoped resources a symbol may need —
/// brushes, fonts, sprites, images. It must not be a tile loader: names
/// it does not have are answered with the synthetic feature, which is
/// the whole mechanism.
///
/// `cache` may be shared across the entries of a legend; entries that
/// share upstream nodes then share the work. Sharing is safe because the
/// entry's identity reaches the cache key — see [`SwatchLoader::hash`].
pub fn render_swatch(
    doc: &Document,
    entry: &LegendEntry,
    registry: &NodeRegistry,
    assets: &dyn AssetLoader,
    params: &ParamValues,
    cache: &Cache,
    opts: &SwatchOptions,
) -> Result<(Arc<RasterBuf>, CanvasInfo), SwatchError> {
    let src = entry.from.as_str();
    let sub = doc.subgraph(src).ok_or_else(|| SwatchError::UnknownNode {
        label: entry.label.clone(),
        src: src.to_string(),
    })?;
    let graph = build_graph(&sub, registry).map_err(|e| SwatchError::Build {
        label: entry.label.clone(),
        source: Box::new(e),
    })?;

    // The entry's own choice wins; the option is the default for the
    // entries that do not make one.
    let geometry = entry.geometry.unwrap_or(opts.geometry);
    let loader = SwatchLoader::new(assets, stand_in_layer(entry, geometry), entry);
    let ev = Evaluator::new(&graph, cache, &loader);
    let required = graph.required_pad().unwrap_or(0);
    let canvas = CanvasInfo {
        tile_w: opts.width,
        tile_h: opts.height,
        pad: opts.pad.max(required),
    };
    // The middle of the world: a latitude where Mercator's scale is 1,
    // so an area-based symbol reads as it does near the equator, and a
    // zoom the caller chose.
    let n = 1u32 << opts.zoom;
    let tile = TileId {
        z: opts.zoom,
        x: n / 2,
        y: n / 2,
    };
    let out = ev
        .render(tile, canvas, params, 0)
        .map_err(|e| SwatchError::Render {
            label: entry.label.clone(),
            source: Box::new(e),
        })?;
    match out {
        PortValue::Raster(r) => Ok((r, canvas)),
        other => Err(SwatchError::NotRaster {
            label: entry.label.clone(),
            src: src.to_string(),
            got: format!("{:?}", other.kind()),
        }),
    }
}

/// How many vertices the stand-in line carries.
///
/// Not two. A brush stroke is painted by walking the polyline's vertices
/// and handing each to the brush as one event; the dabs in between are
/// interpolated from the distance and the time step. Real road geometry
/// arrives finely sampled, so the dab train is dense. A line made of two
/// points is one enormous hop, which emits almost nothing — a swatch
/// that came out blank at legend sizes and visible only when the canvas
/// grew, because the hop grew with it.
const LINE_VERTICES: usize = 64;

/// The stand-in feature a swatch is drawn from: one feature filling the
/// swatch, carrying the entry's declared properties.
///
/// Public because a host drawing its own swatches needs the same
/// stand-in to get the same answer as `ezu legend` does.
pub fn stand_in_layer(entry: &LegendEntry, geometry: LegendGeometry) -> FeatureLayer {
    let e = EXTENT as i32;
    let mid = e / 2;
    let mut g = Geometry::default();
    if matches!(geometry, LegendGeometry::All | LegendGeometry::Polygon) {
        g.polygons.push(Polygon {
            exterior: vec![(0, 0), (e, 0), (e, e), (0, e), (0, 0)],
            holes: vec![],
        });
    }
    if matches!(geometry, LegendGeometry::All | LegendGeometry::Line) {
        let last = LINE_VERTICES - 1;
        g.lines.push(
            (0..LINE_VERTICES)
                .map(|i| ((e as i64 * i as i64 / last as i64) as i32, mid))
                .collect(),
        );
    }
    if matches!(geometry, LegendGeometry::All | LegendGeometry::Point) {
        g.points.push((mid, mid));
    }
    FeatureLayer {
        name: "legend".to_string(),
        extent: EXTENT,
        features: vec![Feature {
            id: None,
            geometry: g,
            properties: properties_of(entry),
        }],
    }
}

/// The entry's declared properties as feature values. Numbers keep their
/// integer-ness where they have it, since `["get", …]` comparisons can
/// see the difference. Arrays and objects are dropped: a feature
/// property is a scalar.
fn properties_of(entry: &LegendEntry) -> HashMap<String, FeatureValue> {
    let mut out = HashMap::new();
    for (k, v) in &entry.properties {
        let value = match v {
            serde_json::Value::String(s) => FeatureValue::String(s.clone()),
            serde_json::Value::Bool(b) => FeatureValue::Bool(*b),
            serde_json::Value::Null => FeatureValue::Null,
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => FeatureValue::Int(i),
                None => FeatureValue::Double(n.as_f64().unwrap_or(0.0)),
            },
            _ => continue,
        };
        out.insert(k.clone(), value);
    }
    out
}

/// Answers every feature request with the swatch's synthetic layer, and
/// everything else from the document's own assets.
///
/// The split is by the shape of the name, the same test `TileLoader`
/// makes: a tile-scoped binding never carries a `scheme:`, and an asset
/// src always does — a bare relative path is refused as "missing a
/// scheme" — so anything without one is a feature layer to stand in for.
struct SwatchLoader<'a> {
    base: &'a dyn AssetLoader,
    features: OpaqueValue,
    /// Folded into every synthetic answer's cache hash. Without the
    /// entry's own identity in here, two entries differing only in their
    /// properties would read each other's cached buffers and every class
    /// of a choropleth would come out the same colour.
    hash: u128,
}

impl<'a> SwatchLoader<'a> {
    fn new(base: &'a dyn AssetLoader, layer: FeatureLayer, entry: &LegendEntry) -> Self {
        let mut h = Xxh3::new();
        h.update(entry.from.as_str().as_bytes());
        // Properties come from a `serde_json::Map`, which orders its keys,
        // so this is stable across runs.
        for (k, v) in &entry.properties {
            h.update(k.as_bytes());
            h.update(v.to_string().as_bytes());
        }
        Self {
            base,
            features: Arc::new(SharedLayer::new(layer)) as Arc<dyn Any + Send + Sync>,
            hash: h.digest128(),
        }
    }
}

impl AssetLoader for SwatchLoader<'_> {
    fn load(&self, name: &str) -> Result<Asset, AssetError> {
        if looks_like_asset_src(name) {
            return self.base.load(name);
        }
        Ok(Asset::Features(self.features.clone()))
    }
    fn hash(&self, name: &str) -> u128 {
        if looks_like_asset_src(name) {
            return self.base.hash(name);
        }
        self.hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ezu_style::NodeRef;

    fn entry(props: &[(&str, serde_json::Value)]) -> LegendEntry {
        let mut properties = serde_json::Map::new();
        for (k, v) in props {
            properties.insert((*k).to_string(), v.clone());
        }
        LegendEntry {
            label: "e".into(),
            from: NodeRef("n".into()),
            properties,
            note: None,
            min_zoom: None,
            max_zoom: None,
            geometry: None,
        }
    }

    /// The regression behind `LINE_VERTICES`: a two-point line is one
    /// hop, and a brush walking it emits next to nothing, so a
    /// brush-stroked entry came out blank at legend sizes.
    #[test]
    fn the_stand_in_line_is_finely_sampled() {
        let layer = stand_in_layer(&entry(&[]), LegendGeometry::Line);
        let line = &layer.features[0].geometry.lines[0];
        assert!(
            line.len() >= 32,
            "a brush needs many events along a stroke, got {}",
            line.len()
        );
        // Spanning the full extent, along the middle, in order.
        assert_eq!(line.first().unwrap().0, 0);
        assert_eq!(line.last().unwrap().0, EXTENT as i32);
        assert!(line.iter().all(|&(_, y)| y == EXTENT as i32 / 2));
        assert!(line.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn geometry_selects_what_the_stand_in_carries() {
        for (geometry, polys, lines, points) in [
            (LegendGeometry::All, 1, 1, 1),
            (LegendGeometry::Polygon, 1, 0, 0),
            (LegendGeometry::Line, 0, 1, 0),
            (LegendGeometry::Point, 0, 0, 1),
        ] {
            let layer = stand_in_layer(&entry(&[]), geometry);
            let g = &layer.features[0].geometry;
            assert_eq!(
                (g.polygons.len(), g.lines.len(), g.points.len()),
                (polys, lines, points),
                "{geometry:?}"
            );
        }
    }

    /// Numbers keep their integer-ness, because `["get", …]` comparisons
    /// can tell the difference. Arrays and objects are dropped — a
    /// feature property is a scalar.
    #[test]
    fn properties_become_feature_values() {
        let layer = stand_in_layer(
            &entry(&[
                ("cls", "trunk".into()),
                ("min_zoom", 0.into()),
                ("density", 12.5.into()),
                ("on", true.into()),
                ("nope", serde_json::json!([1, 2])),
            ]),
            LegendGeometry::All,
        );
        let p = &layer.features[0].properties;
        assert!(matches!(p.get("cls"), Some(FeatureValue::String(s)) if s == "trunk"));
        assert!(matches!(p.get("min_zoom"), Some(FeatureValue::Int(0))));
        assert!(matches!(p.get("density"), Some(FeatureValue::Double(d)) if *d == 12.5));
        assert!(matches!(p.get("on"), Some(FeatureValue::Bool(true))));
        assert!(p.get("nope").is_none(), "an array is not a property value");
    }
}
