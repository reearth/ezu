//! Topology / type-checking / pad-propagation tests over the graph
//! builder. No JSON parsing, no evaluation.

use super::common::{passthrough, src, Mock};
use crate::{BuildError, GraphBuilder, PortKind, PortSpec};

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
