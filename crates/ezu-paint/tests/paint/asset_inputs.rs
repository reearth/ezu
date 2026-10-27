//! Every source node that loads an asset names it in `asset_inputs`, so the
//! asset's hash keys the node's cache entry and a host binding assets up
//! front (wasm) can see what the graph reads.

use ezu_graph::build_graph;

fn asset_inputs(nodes: &str) -> Vec<String> {
    let doc = ezu_style::Document::from_json(&format!(
        r##"{{
      "name": "t", "tile-size": 64,
      "sources": {{
        "pic":   {{ "type": "image", "src": "builtin:pic" }},
        "ink":   {{ "type": "brush", "src": "builtin:ink" }},
        "sheet": {{ "type": "sprite", "image": "builtin:atlas",
                    "index": {{ "dot": {{ "x": 0, "y": 0, "width": 4, "height": 4 }} }} }}
      }},
      "nodes": {{ {nodes} }},
      "output": "@out"
    }}"##
    ))
    .expect("parse");
    let graph = build_graph(&doc, &ezu_paint::nodes::default_registry()).expect("build");
    graph.asset_inputs().into_iter().collect()
}

#[test]
fn image_icon_and_brush_file_name_the_assets_they_load() {
    let pts = r#""pts": { "op": "point-grid", "spacing": 1024 }"#;
    assert_eq!(
        asset_inputs(&format!(
            r##"{pts}, "img": {{ "op": "image", "src": "@pic" }},
               "out": {{ "op": "stamp", "features": "@pts", "image": "@img" }}"##
        )),
        ["builtin:pic"]
    );
    assert_eq!(
        asset_inputs(&format!(
            r##"{pts}, "img": {{ "op": "icon", "sprite": "@sheet", "name": "dot" }},
               "out": {{ "op": "stamp", "features": "@pts", "image": "@img" }}"##
        )),
        ["builtin:atlas"]
    );
    assert_eq!(
        asset_inputs(&format!(
            r##"{pts}, "brush": {{ "op": "brush-file", "src": "@ink" }},
               "out": {{ "op": "line", "features": "@pts", "brush": "@brush", "color": "#000000" }}"##
        )),
        ["builtin:ink"]
    );
}
