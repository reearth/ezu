//! Geometry ops — `Features -> Features` transforms (centroid, buffer,
//! simplify, …) inspired by turf.js, plus `contour`, which extracts
//! isoline `Features` from a `ScalarField`.

mod bbox;
mod boundary;
mod buffer;
mod centroid;
mod contour;
mod convex_hull;
mod dash;
mod dot_density;
mod feature_boolean;
mod hatch;
mod junctions;
mod medial_axis;
mod resample;
mod segment_to;
mod simplify;
mod transform;
mod triangulate;
mod voronoi;
mod voronoi_fracture;
mod wave;

/// Build a geometry or source node from its style fields and run it on
/// a plain or a denser canvas, for tests comparing pixel ratios.
#[cfg(test)]
pub(crate) mod ratio_test {
    use std::sync::Arc;

    use ezu_graph::{
        CanvasInfo, EvalCtx, NoAssets, Node, NodeFactory, ParamValues, PortValue, TileId, Warnings,
    };

    use crate::nodes::common::{downcast_features, features_value, FeatureGroup, FilteredFeatures};

    pub(crate) const EXTENT: u32 = 4096;

    /// A tile away from the origin, so world anchoring is exercised.
    pub(crate) const TILE: TileId = TileId { z: 5, x: 13, y: 9 };

    /// The plain canvas: a 256 px tile with 8 px of pad.
    pub(crate) fn plain() -> CanvasInfo {
        CanvasInfo::square(256, 8)
    }

    /// The same tile at twice the density: every device size doubles.
    pub(crate) fn retina() -> CanvasInfo {
        CanvasInfo::square(512, 16).with_pixel_ratio(2.0)
    }

    pub(crate) fn build(factory: &dyn NodeFactory, fields: serde_json::Value) -> Box<dyn Node> {
        let params = Default::default();
        let sources = Default::default();
        let warnings = Warnings::new();
        let ctx = ezu_graph::FactoryCtx {
            params: &params,
            sources: &sources,
            node: "n",
            warnings: &warnings,
        };
        factory
            .build(fields.as_object().expect("fields object"), &ctx)
            .expect("node builds")
            .node
    }

    /// One feature group in [`EXTENT`] units, as a `features` input.
    pub(crate) fn input(group: FeatureGroup) -> Option<PortValue> {
        Some(features_value(EXTENT, vec![group]))
    }

    pub(crate) fn eval(
        node: &dyn Node,
        canvas: CanvasInfo,
        inputs: &[Option<PortValue>],
    ) -> Arc<FilteredFeatures> {
        let params = ParamValues::new();
        let assets = NoAssets;
        let ctx = EvalCtx {
            tile: TILE,
            canvas,
            assets: &assets,
            params: &params,
            rng_seed: 0,
            influence_pad: u32::MAX,
        };
        downcast_features(&node.eval(&ctx, inputs).expect("node evaluates")).expect("features")
    }

    /// One polygon as its rings, exterior first.
    pub(crate) type Rings = Vec<Vec<(i32, i32)>>;

    /// One group's polygons, lines and points.
    pub(crate) type Group = (Vec<Rings>, Rings, Vec<(i32, i32)>);

    /// Every group's geometry, for comparing two outputs whole.
    pub(crate) fn geometry(f: &FilteredFeatures) -> Vec<Group> {
        f.groups
            .iter()
            .map(|g| {
                let polygons = g
                    .polygons
                    .iter()
                    .map(|p| {
                        std::iter::once(p.exterior.clone())
                            .chain(p.holes.iter().cloned())
                            .collect()
                    })
                    .collect();
                (polygons, g.lines.clone(), g.points.clone())
            })
            .collect()
    }
}
