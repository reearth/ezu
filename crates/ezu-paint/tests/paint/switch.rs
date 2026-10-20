//! `switch` with more than two branches — by name (`branches`) and by
//! condition (`cases`) — and the rule every form keeps: a branch that is
//! not picked is not rendered.

use crate::common::{render, render_with_params};

use ezu_graph::{
    build_graph, Cache, CanvasInfo, Evaluator, NoAssets, ParamValues, ScalarValue, TileId,
};
use ezu_paint::nodes::default_registry;
use ezu_style::Document;

const Z0: TileId = TileId { z: 0, x: 0, y: 0 };
const RED: [u8; 4] = [0xff, 0x00, 0x00, 0xff];
const GREEN: [u8; 4] = [0x00, 0xff, 0x00, 0xff];
const BLUE: [u8; 4] = [0x00, 0x00, 0xff, 0xff];

/// Three solids, a `boom` node that fails whenever it is evaluated, and a
/// switch over them spliced in as `out`. `boom` and the `zero` it reads are
/// left out unless `out` names one: a node nothing reads still renders.
fn doc(params: &str, out: &str) -> String {
    let mut failing = String::new();
    if out.contains("@boom") || out.contains("@zero") {
        failing.push_str(r#""zero": { "op": "math", "fn": "div", "a": 1, "b": 0 },"#);
    }
    if out.contains("@boom") {
        failing.push_str(
            r#""boom": { "op": "blend", "base": "@red", "over": "@red", "opacity": "@zero" },"#,
        );
    }
    format!(
        r##"{{
          "name": "switch",
          "tile-size": 8,
          "params": {{ {params} }},
          "nodes": {{
            "red":   {{ "op": "solid", "color": "#ff0000" }},
            "green": {{ "op": "solid", "color": "#00ff00" }},
            "blue":  {{ "op": "solid", "color": "#0000ff" }},
            {failing}
            "out":   {{ "op": "switch", {out} }}
          }},
          "output": "@out"
        }}"##
    )
}

fn string(s: &str) -> ScalarValue {
    ScalarValue::String(s.into())
}

fn build_error(json: &str) -> String {
    let doc = Document::from_json(json).expect("parse");
    match build_graph(&doc, &default_registry()) {
        Ok(_) => panic!("built"),
        Err(e) => e.to_string(),
    }
}

/// The render's error, for a style that should fail.
fn render_error(json: &str, params: &[(&str, ScalarValue)]) -> String {
    let doc = Document::from_json(json).expect("parse");
    let graph = build_graph(&doc, &default_registry()).expect("build");
    let cache = Cache::new();
    let mut pv = ParamValues::new();
    for (k, v) in params {
        pv.set(k.to_string(), v.clone());
    }
    match Evaluator::new(&graph, &cache, &NoAssets).render(Z0, CanvasInfo::square(8, 0), &pv, 0) {
        Ok(_) => panic!("rendered"),
        Err(e) => e.to_string(),
    }
}

const THEME: &str =
    r#""theme": { "type": "string", "enum": ["warm", "cool", "plain"], "default": "warm" }"#;

#[test]
fn branches_are_picked_by_a_string_param() {
    let json = doc(
        THEME,
        r#""branches": { "warm": "@red", "cool": "@blue", "plain": "@green" }, "select": "$theme""#,
    );
    assert_eq!(render(&json, 8, 0).pixel(4, 4), RED);
    for (theme, want) in [("cool", BLUE), ("plain", GREEN), ("warm", RED)] {
        let r = render_with_params(&json, 8, 0, Z0, &[("theme", string(theme))]);
        assert_eq!(r.pixel(4, 4), want, "{theme}");
    }
}

#[test]
fn a_literal_name_or_an_unlisted_one_takes_default() {
    let branches = r#""branches": { "warm": "@red", "cool": "@blue" }"#;
    let json = doc("", &format!(r#"{branches}, "select": "cool""#));
    assert_eq!(render(&json, 8, 0).pixel(4, 4), BLUE);
    let json = doc(
        "",
        &format!(r#"{branches}, "select": "sepia", "default": "@green""#),
    );
    assert_eq!(render(&json, 8, 0).pixel(4, 4), GREEN);
    // A param without an `enum` may name anything, so it needs a default.
    let any = r#""theme": { "type": "string", "default": "warm" }"#;
    let json = doc(
        any,
        &format!(r#"{branches}, "select": "$theme", "default": "@green""#),
    );
    let r = render_with_params(&json, 8, 0, Z0, &[("theme", string("sepia"))]);
    assert_eq!(r.pixel(4, 4), GREEN);
}

#[test]
fn a_name_that_can_reach_no_branch_fails_the_build() {
    let branches = r#""branches": { "warm": "@red", "cool": "@blue" }"#;
    // `plain` is allowed but has no branch.
    let msg = build_error(&doc(THEME, &format!(r#"{branches}, "select": "$theme""#)));
    assert!(msg.contains("`plain`, which names no branch"), "{msg}");
    // No `enum`: anything could arrive.
    let any = r#""theme": { "type": "string", "default": "warm" }"#;
    let msg = build_error(&doc(any, &format!(r#"{branches}, "select": "$theme""#)));
    assert!(msg.contains("declares no `enum`"), "{msg}");
    // A literal with no branch and no default.
    let msg = build_error(&doc("", &format!(r#"{branches}, "select": "sepia""#)));
    assert!(msg.contains("`sepia` names no branch"), "{msg}");
    // Not a string param.
    let flag = r#""theme": { "type": "bool", "default": true }"#;
    let msg = build_error(&doc(flag, &format!(r#"{branches}, "select": "$theme""#)));
    assert!(msg.contains("needs a `string` param"), "{msg}");
    // A node cannot name a branch.
    let msg = build_error(&doc("", &format!(r#"{branches}, "select": "@red""#)));
    assert!(msg.contains("no node produces a string"), "{msg}");
}

const FLAGS: &str = r#""night": { "type": "bool", "default": false },
                       "snow":  { "type": "bool", "default": false }"#;

#[test]
fn cases_take_the_first_that_holds_else_default() {
    let json = doc(
        FLAGS,
        r#""cases": [ { "when": "$night", "then": "@blue" },
                      { "when": "$snow",  "then": "@green" } ],
           "default": "@red""#,
    );
    let at = |night: bool, snow: bool| {
        render_with_params(
            &json,
            8,
            0,
            Z0,
            &[
                ("night", ScalarValue::Bool(night)),
                ("snow", ScalarValue::Bool(snow)),
            ],
        )
        .pixel(4, 4)
    };
    assert_eq!(at(false, false), RED);
    assert_eq!(at(false, true), GREEN);
    // Both hold: the first wins.
    assert_eq!(at(true, true), BLUE);
}

/// A condition can be computed: an `expr` over the zoom, say.
#[test]
fn a_case_can_hang_on_a_node() {
    let json = doc(
        "",
        r#""cases": [ { "when": "@deep", "then": "@blue" } ], "default": "@red""#,
    )
    .replace(
        r#""out":"#,
        r#""deep": { "op": "expr", "type": "bool", "expr": ["step", ["zoom"], false, 10, true] }, "out":"#,
    );
    assert_eq!(render(&json, 8, 0).pixel(4, 4), RED);
    let deep = crate::common::render_tile(&json, 8, 0, TileId { z: 12, x: 0, y: 0 });
    assert_eq!(deep.pixel(4, 4), BLUE);
}

/// Literal conditions decide when the graph is built, so the branches may
/// be different kinds; a per-render one needs them all alike.
#[test]
fn literal_cases_let_branches_differ_in_kind() {
    let json = doc(
        "",
        r#""cases": [ { "when": false, "then": "@zero" }, { "when": true, "then": "@green" } ],
           "default": "@red""#,
    );
    assert_eq!(render(&json, 8, 0).pixel(4, 4), GREEN);
    let msg = build_error(&doc(
        FLAGS,
        r#""cases": [ { "when": "$night", "then": "@zero" } ], "default": "@red""#,
    ));
    assert!(msg.contains("every branch must be the same kind"), "{msg}");
}

/// `boom` fails whenever it renders, so each of these renders only if the
/// branch holding it is left alone.
#[test]
fn a_branch_not_picked_is_not_rendered() {
    // Sanity: picking it does fail.
    let picked = doc("", r#""a": "@boom", "b": "@red""#);
    assert!(render_error(&picked, &[]).contains("non-finite"));

    let two_way = doc(
        r#""flip": { "type": "bool", "default": false }"#,
        r#""a": "@red", "b": "@boom", "select": "$flip""#,
    );
    assert_eq!(render(&two_way, 8, 0).pixel(4, 4), RED);
    assert!(render_error(&two_way, &[("flip", ScalarValue::Bool(true))]).contains("non-finite"));

    let literal = doc("", r#""a": "@boom", "b": "@red", "select": "b""#);
    assert_eq!(render(&literal, 8, 0).pixel(4, 4), RED);

    let by_name = doc(
        THEME,
        r#""branches": { "warm": "@red", "cool": "@boom", "plain": "@green" }, "select": "$theme""#,
    );
    assert_eq!(render(&by_name, 8, 0).pixel(4, 4), RED);
    let r = render_with_params(&by_name, 8, 0, Z0, &[("theme", string("plain"))]);
    assert_eq!(r.pixel(4, 4), GREEN);

    let cases = doc(
        FLAGS,
        r#""cases": [ { "when": "$night", "then": "@boom" }, { "when": "$snow", "then": "@green" } ],
           "default": "@boom""#,
    );
    let r = render_with_params(&cases, 8, 0, Z0, &[("snow", ScalarValue::Bool(true))]);
    assert_eq!(r.pixel(4, 4), GREEN);
}

#[test]
fn a_string_param_names_a_or_b() {
    let json = doc(
        r#""side": { "type": "string", "enum": ["a", "b"], "default": "b" }"#,
        r#""a": "@red", "b": "@blue", "select": "$side""#,
    );
    assert_eq!(render(&json, 8, 0).pixel(4, 4), BLUE);
    let r = render_with_params(&json, 8, 0, Z0, &[("side", string("a"))]);
    assert_eq!(r.pixel(4, 4), RED);
}

#[test]
fn the_forms_do_not_mix() {
    let msg = build_error(&doc(
        "",
        r#""a": "@red", "b": "@blue", "cases": [ { "when": true, "then": "@green" } ], "default": "@red""#,
    ));
    assert!(msg.contains("different forms"), "{msg}");
    let msg = build_error(&doc(
        "",
        r#""a": "@red", "b": "@blue", "default": "@green""#,
    ));
    assert!(msg.contains("`default` belongs to"), "{msg}");
    let msg = build_error(&doc(
        "",
        r#""cases": [ { "when": true, "then": "@green" } ]"#,
    ));
    assert!(msg.contains("default"), "{msg}");
}
