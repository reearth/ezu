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

    use crate::nodes::common::{
        downcast_features, features_port, features_value, FeatureGroup, FilteredFeatures,
        NeighbourLayer,
    };

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
        let nodes = Default::default();
        let warnings = Warnings::new();
        let ctx = ezu_graph::FactoryCtx {
            params: &params,
            sources: &sources,
            nodes: &nodes,
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
        eval_at(node, canvas, TILE, false, inputs)
    }

    /// [`eval`] at `tile`, asked for neighbours or not.
    pub(crate) fn eval_at(
        node: &dyn Node,
        canvas: CanvasInfo,
        tile: TileId,
        neighbours: bool,
        inputs: &[Option<PortValue>],
    ) -> Arc<FilteredFeatures> {
        let params = ParamValues::new();
        let assets = NoAssets;
        let ctx = EvalCtx {
            tile,
            canvas,
            assets: &assets,
            params: &params,
            rng_seed: 0,
            influence_pad: u32::MAX,
            neighbours,
        };
        downcast_features(&node.eval(&ctx, inputs).expect("node evaluates")).expect("features")
    }

    /// One group in [`EXTENT`] units as a `features` input, carrying
    /// `neighbour` as the tile `(dx, dy)` away.
    pub(crate) fn input_with_neighbour(
        group: FeatureGroup,
        dx: i8,
        dy: i8,
        neighbour: FeatureGroup,
    ) -> Option<PortValue> {
        Some(features_port(
            FilteredFeatures::new(EXTENT, vec![group]).with_neighbours(vec![NeighbourLayer::new(
                dx,
                dy,
                EXTENT,
                vec![neighbour],
            )]),
        ))
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

#[cfg(test)]
mod neighbour_tests {
    use super::ratio_test::{
        build, eval_at, geometry, input, input_with_neighbour, plain, EXTENT, TILE,
    };
    use crate::nodes::common::FeatureGroup;
    use ezu_features::Polygon;
    use ezu_graph::TileId;

    fn square(x0: i32, y0: i32, x1: i32, y1: i32) -> FeatureGroup {
        FeatureGroup::synthetic(
            vec![Polygon {
                exterior: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)],
                holes: vec![],
            }],
            vec![],
            vec![],
        )
    }

    /// An op anchored to the world grid lays a neighbour out exactly as
    /// the neighbour's own render does, which is what lets a label node
    /// compare the two.
    #[test]
    fn a_per_feature_op_gives_each_neighbour_what_its_own_tile_would() {
        let node = build(
            &super::hatch::HatchFactory,
            serde_json::json!({ "features": "@f", "spacing": 7, "angle-deg": 30 }),
        );
        let own = square(100, 100, 3000, 2900);
        let east = square(-200, 500, 2500, 3900);
        let got = eval_at(
            node.as_ref(),
            plain(),
            TILE,
            true,
            &[input_with_neighbour(own, 1, 0, east)],
        );
        assert_eq!(got.neighbours.len(), 1);
        let n = &got.neighbours[0];
        assert_eq!((n.dx, n.dy, n.extent), (1, 0, EXTENT));
        let as_own = eval_at(
            node.as_ref(),
            plain(),
            TileId {
                x: TILE.x + 1,
                ..TILE
            },
            false,
            &[input(square(-200, 500, 2500, 3900))],
        );
        let neighbour_lines: Vec<_> = n.groups.iter().flat_map(|g| g.lines.clone()).collect();
        let own_lines: Vec<_> = as_own.lines().cloned().collect();
        assert!(!own_lines.is_empty());
        assert_eq!(neighbour_lines, own_lines);
        assert_eq!(
            geometry(&got).len(),
            1,
            "the tile's own groups stay its own"
        );
    }

    #[test]
    fn neighbours_pass_through_only_when_asked_for() {
        let node = build(
            &super::centroid::CentroidFactory,
            serde_json::json!({ "features": "@f" }),
        );
        let f = input_with_neighbour(square(0, 0, 10, 10), 0, 1, square(0, 0, 10, 10));
        let off = eval_at(
            node.as_ref(),
            plain(),
            TILE,
            false,
            std::slice::from_ref(&f),
        );
        assert!(off.neighbours.is_empty());
        let on = eval_at(node.as_ref(), plain(), TILE, true, std::slice::from_ref(&f));
        assert_eq!(on.neighbours.len(), 1);
        assert_eq!(on.neighbours[0].groups[0].points, vec![(5, 5)]);
    }

    /// An op merging features cannot say what a neighbour tile would make
    /// of them, so the neighbours stop there.
    #[test]
    fn a_merging_op_drops_neighbours() {
        let node = build(
            &super::convex_hull::ConvexHullFactory,
            serde_json::json!({ "features": "@f" }),
        );
        let f = input_with_neighbour(square(0, 0, 10, 10), 1, 0, square(0, 0, 10, 10));
        let out = eval_at(node.as_ref(), plain(), TILE, true, &[f]);
        assert!(out.neighbours.is_empty());
        assert!(!node.forwards_neighbours(0));
    }
}
