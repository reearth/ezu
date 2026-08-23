//! Topology / type-checking / pad-propagation tests over the graph
//! builder. No JSON parsing, no evaluation.

use super::common::{passthrough, src, Mock};
use crate::{BuildError, GraphBuilder, InfluenceCtx, InkReach, NoAssets, PortKind, PortSpec};

#[test]
fn linear_chain_topo() {
    let mut b = GraphBuilder::new();
    b.add_node("a", src(PortKind::Raster))
        .add_node("b", passthrough(PortKind::Raster, PortKind::Raster))
        .add_node("c", passthrough(PortKind::Raster, PortKind::Raster))
        .connect("a", "b", "input")
        .connect("b", "c", "input")
        .set_output("c");
    let g = b.build().unwrap();
    let order: Vec<_> = g.topo_order().iter().map(|&i| g.node_id(i)).collect();
    assert_eq!(order, vec!["a", "b", "c"]);
}

#[test]
fn diamond_topo() {
    // a -> b, a -> c, b+c -> d
    let merge = Mock::new(
        "merge",
        vec![
            PortSpec::new("left", &[PortKind::Raster]),
            PortSpec::new("right", &[PortKind::Raster]),
        ],
        PortKind::Raster,
    )
    .boxed();

    let mut b = GraphBuilder::new();
    b.add_node("a", src(PortKind::Raster))
        .add_node("b", passthrough(PortKind::Raster, PortKind::Raster))
        .add_node("c", passthrough(PortKind::Raster, PortKind::Raster))
        .add_node("d", merge)
        .connect("a", "b", "input")
        .connect("a", "c", "input")
        .connect("b", "d", "left")
        .connect("c", "d", "right")
        .set_output("d");
    let g = b.build().unwrap();
    let order: Vec<_> = g.topo_order().iter().map(|&i| g.node_id(i)).collect();
    assert_eq!(order[0], "a");
    assert_eq!(order[3], "d");
    // b and c can appear in either order, both before d.
    assert!(order.contains(&"b"));
    assert!(order.contains(&"c"));
}

#[test]
fn type_mismatch_is_rejected() {
    let mut b = GraphBuilder::new();
    b.add_node("a", src(PortKind::Raster))
        .add_node("b", passthrough(PortKind::Brush, PortKind::Brush))
        .connect("a", "b", "input")
        .set_output("b");
    match b.build() {
        Err(BuildError::TypeMismatch { accepts, got, .. }) => {
            assert_eq!(accepts, vec![PortKind::Brush]);
            assert_eq!(got, PortKind::Raster);
        }
        other => panic!("expected TypeMismatch, got {other:?}"),
    }
}

#[test]
fn missing_required_port_is_rejected() {
    let mut b = GraphBuilder::new();
    b.add_node("b", passthrough(PortKind::Raster, PortKind::Raster))
        .set_output("b");
    match b.build() {
        Err(BuildError::MissingInput { node, port }) => {
            assert_eq!(node, "b");
            assert_eq!(port, "input");
        }
        other => panic!("expected MissingInput, got {other:?}"),
    }
}

#[test]
fn optional_port_may_be_unconnected() {
    let opt = Mock::new(
        "opt",
        vec![
            PortSpec::new("input", &[PortKind::Raster]),
            PortSpec::new("extra", &[PortKind::Raster]).optional(),
        ],
        PortKind::Raster,
    )
    .boxed();
    let mut b = GraphBuilder::new();
    b.add_node("a", src(PortKind::Raster))
        .add_node("o", opt)
        .connect("a", "o", "input")
        .set_output("o");
    let g = b.build().unwrap();
    assert_eq!(g.len(), 2);
}

#[test]
fn cycle_is_detected() {
    // a -> b -> a; pure cycle (no entry node) is unreachable from output,
    // but is still rejected.
    let mut b = GraphBuilder::new();
    b.add_node("a", passthrough(PortKind::Raster, PortKind::Raster))
        .add_node("b", passthrough(PortKind::Raster, PortKind::Raster))
        .connect("a", "b", "input")
        .connect("b", "a", "input")
        .set_output("b");
    match b.build() {
        Err(BuildError::Cycle(_)) => {}
        other => panic!("expected Cycle, got {other:?}"),
    }
}

#[test]
fn unknown_port_name_is_rejected() {
    let mut b = GraphBuilder::new();
    b.add_node("a", src(PortKind::Raster))
        .add_node("b", passthrough(PortKind::Raster, PortKind::Raster))
        .connect("a", "b", "nope")
        .set_output("b");
    match b.build() {
        Err(BuildError::UnknownPort { node, port }) => {
            assert_eq!(node, "b");
            assert_eq!(port, "nope");
        }
        other => panic!("expected UnknownPort, got {other:?}"),
    }
}

#[test]
fn duplicate_edge_is_rejected() {
    let mut b = GraphBuilder::new();
    b.add_node("a", src(PortKind::Raster))
        .add_node("c", src(PortKind::Raster))
        .add_node("b", passthrough(PortKind::Raster, PortKind::Raster))
        .connect("a", "b", "input")
        .connect("c", "b", "input")
        .set_output("b");
    match b.build() {
        Err(BuildError::DuplicateEdge { .. }) => {}
        other => panic!("expected DuplicateEdge, got {other:?}"),
    }
}

#[test]
fn pad_propagates_upstream_through_blur() {
    // src -> blur(grow=24) -> out(passthrough)
    let mut b = GraphBuilder::new();
    b.add_node("src", src(PortKind::Raster))
        .add_node(
            "blur",
            Mock::new(
                "blur",
                vec![PortSpec::new("input", &[PortKind::Raster])],
                PortKind::Raster,
            )
            .with_pad_grow(24)
            .boxed(),
        )
        .add_node("out", passthrough(PortKind::Raster, PortKind::Raster))
        .connect("src", "blur", "input")
        .connect("blur", "out", "input")
        .set_output("out");
    let g = b.build().unwrap();
    let pads = g.compute_pad(8).unwrap();
    // out: 8 (doc pad). blur upstream of out, so it sees 8 then declares
    // 32 upstream. src therefore needs 32.
    let p = |id: &str| {
        let ix = g
            .topo_order()
            .iter()
            .find(|&&i| g.node_id(i) == id)
            .unwrap();
        pads[*ix]
    };
    assert_eq!(p("out"), 8);
    assert_eq!(p("blur"), 8);
    assert_eq!(p("src"), 32);
}

#[test]
fn pad_exceeded_errors() {
    let mut b = GraphBuilder::new();
    b.add_node("src", src(PortKind::Raster))
        .add_node(
            "blur",
            Mock::new(
                "blur",
                vec![PortSpec::new("input", &[PortKind::Raster])],
                PortKind::Raster,
            )
            .with_pad_grow(crate::MAX_PAD + 1)
            .boxed(),
        )
        .connect("src", "blur", "input")
        .set_output("blur");
    let g = b.build().unwrap();
    match g.compute_pad(0) {
        Err(BuildError::PadExceeded { .. }) => {}
        other => panic!("expected PadExceeded, got {other:?}"),
    }
}

#[test]
fn order_finishes_one_branch_before_starting_the_next() {
    // Two independent chains feeding a merge. A breadth-first order
    // would interleave them and hold both chains' buffers at once; the
    // evaluation order should instead complete one chain, letting its
    // intermediates go, before touching the other.
    let merge = Mock::new(
        "merge",
        vec![
            PortSpec::new("left", &[PortKind::Raster]),
            PortSpec::new("right", &[PortKind::Raster]),
        ],
        PortKind::Raster,
    )
    .boxed();

    let mut b = GraphBuilder::new();
    b.add_node("l0", src(PortKind::Raster))
        .add_node("l1", passthrough(PortKind::Raster, PortKind::Raster))
        .add_node("r0", src(PortKind::Raster))
        .add_node("r1", passthrough(PortKind::Raster, PortKind::Raster))
        .add_node("m", merge)
        .connect("l0", "l1", "input")
        .connect("r0", "r1", "input")
        .connect("l1", "m", "left")
        .connect("r1", "m", "right")
        .set_output("m");
    let g = b.build().unwrap();
    let order: Vec<_> = g.topo_order().iter().map(|&i| g.node_id(i)).collect();
    assert_eq!(order, vec!["l0", "l1", "r0", "r1", "m"]);
}

#[test]
fn nodes_the_output_ignores_are_still_ordered() {
    // `side` reads `a` but nothing reads `side`. It must still appear,
    // and after the node it depends on.
    let mut b = GraphBuilder::new();
    b.add_node("a", src(PortKind::Raster))
        .add_node("out", passthrough(PortKind::Raster, PortKind::Raster))
        .add_node("side", passthrough(PortKind::Raster, PortKind::Raster))
        .connect("a", "out", "input")
        .connect("a", "side", "input")
        .set_output("out");
    let g = b.build().unwrap();
    let order: Vec<_> = g.topo_order().iter().map(|&i| g.node_id(i)).collect();
    assert_eq!(order.len(), 3);
    let pos = |id: &str| order.iter().position(|n| *n == id).unwrap();
    assert!(pos("a") < pos("out"));
    assert!(pos("a") < pos("side"));
}

// ---------------------------------------------------------------------------
// Influence — how far outside the canvas geometry can still matter.

/// The reach accumulates along the chain from the output back to the
/// source, so a source learns what every op between it and the canvas
/// can do with its geometry.
#[test]
fn influence_accumulates_back_to_the_source() {
    let mut b = GraphBuilder::new();
    b.add_node("feat", src(PortKind::Features))
        .add_node(
            "wave",
            Mock::new(
                "wave",
                vec![PortSpec::new("input", &[PortKind::Features])],
                PortKind::Features,
            )
            .with_influence_grow(Some(4))
            .boxed(),
        )
        .add_node(
            "draw",
            Mock::new(
                "draw",
                vec![PortSpec::new("input", &[PortKind::Features])],
                PortKind::Raster,
            )
            .with_influence_grow(Some(10))
            .boxed(),
        )
        .add_node(
            "blur",
            Mock::new(
                "blur",
                vec![PortSpec::new("input", &[PortKind::Raster])],
                PortKind::Raster,
            )
            .with_influence_grow(Some(6))
            .boxed(),
        )
        .connect("feat", "wave", "input")
        .connect("wave", "draw", "input")
        .connect("draw", "blur", "input")
        .set_output("blur");
    let g = b.build().unwrap();
    let pads = g.influence_pads(&NoAssets);
    let at = |id: &str| {
        pads[g
            .topo_order()
            .iter()
            .copied()
            .find(|&i| g.node_id(i) == id)
            .unwrap()]
    };
    assert_eq!(at("blur"), 0, "the output claims nothing beyond itself");
    assert_eq!(at("draw"), 6);
    assert_eq!(at("wave"), 16);
    assert_eq!(at("feat"), 20);
}

/// One op that cannot bound its reach makes everything above it
/// unbounded too — the safe answer, since dropping geometry it might
/// have used would lose ink.
#[test]
fn an_unbounded_op_makes_everything_above_it_unbounded() {
    let mut b = GraphBuilder::new();
    b.add_node("feat", src(PortKind::Features))
        .add_node(
            "shift",
            Mock::new(
                "shift",
                vec![PortSpec::new("input", &[PortKind::Features])],
                PortKind::Features,
            )
            .with_influence_grow(None)
            .boxed(),
        )
        .add_node(
            "draw",
            Mock::new(
                "draw",
                vec![PortSpec::new("input", &[PortKind::Features])],
                PortKind::Raster,
            )
            .with_influence_grow(Some(10))
            .boxed(),
        )
        .connect("feat", "shift", "input")
        .connect("shift", "draw", "input")
        .set_output("draw");
    let g = b.build().unwrap();
    let pads = g.influence_pads(&NoAssets);
    let at = |id: &str| {
        pads[g
            .topo_order()
            .iter()
            .copied()
            .find(|&i| g.node_id(i) == id)
            .unwrap()]
    };
    // `shift` still sees what `draw` claims; it is what lies *above*
    // `shift` that cannot be bounded.
    assert_eq!(at("shift"), 10);
    assert_eq!(at("feat"), InfluenceCtx::UNBOUNDED);
}

/// A source feeding two branches has to satisfy the hungrier one.
#[test]
fn a_shared_source_takes_the_widest_of_its_consumers() {
    let mut b = GraphBuilder::new();
    let branch = |grow: u32| {
        Mock::new(
            "draw",
            vec![PortSpec::new("input", &[PortKind::Features])],
            PortKind::Raster,
        )
        .with_influence_grow(Some(grow))
        .boxed()
    };
    b.add_node("feat", src(PortKind::Features))
        .add_node("narrow", branch(3))
        .add_node("wide", branch(30))
        .add_node(
            "merge",
            Mock::new(
                "merge",
                vec![
                    PortSpec::new("left", &[PortKind::Raster]),
                    PortSpec::new("right", &[PortKind::Raster]),
                ],
                PortKind::Raster,
            )
            .boxed(),
        )
        .connect("feat", "narrow", "input")
        .connect("feat", "wide", "input")
        .connect("narrow", "merge", "left")
        .connect("wide", "merge", "right")
        .set_output("merge");
    let g = b.build().unwrap();
    let pads = g.influence_pads(&NoAssets);
    let at = |id: &str| {
        pads[g
            .topo_order()
            .iter()
            .copied()
            .find(|&i| g.node_id(i) == id)
            .unwrap()]
    };
    assert_eq!(at("feat"), 30);
}

/// A stroking op is handed its brush's reach by the graph, because the
/// brush arrives on a port it cannot read before eval.
#[test]
fn a_brush_hands_its_reach_to_the_op_that_strokes_with_it() {
    let draw = |name: &'static str| {
        Mock::new(
            name,
            vec![
                PortSpec::new("input", &[PortKind::Features]),
                PortSpec::new("brush", &[PortKind::Brush]),
            ],
            PortKind::Raster,
        )
        .with_influence_grow(Some(0))
        .boxed()
    };
    let mut b = GraphBuilder::new();
    b.add_node("feat", src(PortKind::Features))
        .add_node(
            "brush",
            Mock::new("brush", vec![], PortKind::Brush)
                .with_brush_reach(37.0, 4.0)
                .boxed(),
        )
        .add_node("draw", draw("draw"))
        .connect("feat", "draw", "input")
        .connect("brush", "draw", "brush")
        .set_output("draw");
    let g = b.build().unwrap();
    let pads = g.influence_pads(&NoAssets);
    let at = |id: &str| {
        pads[g
            .topo_order()
            .iter()
            .copied()
            .find(|&i| g.node_id(i) == id)
            .unwrap()]
    };
    assert_eq!(at("feat"), 37);
}

/// Overriding a brush's radius scales everything else about the dab
/// with it, so the reach follows.
#[test]
fn a_brushs_reach_scales_with_the_radius_it_is_used_at() {
    let ink = InkReach {
        reach_px: 40.0,
        radius_px: 4.0,
    };
    assert_eq!(ink.at_radius(4.0), 40.0);
    assert_eq!(ink.at_radius(8.0), 80.0);
    // A narrower radius never claims *less* than the brush already
    // reaches — the jitters do not shrink with it.
    assert_eq!(ink.at_radius(1.0), 40.0);
}
