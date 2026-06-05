//! Function expansion: rewrite a [`Document`] with `functions` into a
//! flat node graph before building.
//!
//! Functions behave like hygienic macros over the node graph:
//!
//! - A call node `{ "op": "func", "fn": "f", ...args }` is replaced by
//!   a copy of `f`'s body. Body node ids are namespaced as
//!   `<call-id>/<body-id>`; the body's output node takes the call id
//!   itself, so `@<call-id>` references keep working unchanged.
//! - Arguments substitute structurally: every `@<input-name>` in the
//!   body (recursively, including inside arrays/objects) is replaced
//!   by the caller's argument *value* — a literal stays a literal, a
//!   `$param` stays a runtime-resolved param, a `@node` becomes a port
//!   connection to the caller's graph.
//! - Bodies are closed over their inputs: a body reference must name a
//!   function input, another body node, or a document-scoped source.
//!   Anything else is an error (catches typos, keeps functions
//!   reusable).
//! - Functions may call functions; the call graph must be acyclic
//!   (checked up front, reported with the cycle path). A node-count
//!   cap guards against exponential nesting.
//!
//! Because the evaluator's cache keys are content-addressed (no node
//! identity), two calls with identical arguments share cache entries
//! after expansion — inlining does not duplicate work.

use indexmap::IndexMap;
use serde_json::Value;

use crate::spec::{Document, FuncDecl, FuncKind, NodeSpec, SourceDecl};

/// Hard ceiling on the number of nodes an expanded document may
/// contain. Functions calling functions multiply node counts; this
/// converts a runaway composition into a clear error.
pub const MAX_EXPANDED_NODES: usize = 10_000;

#[derive(Debug, thiserror::Error)]
pub enum ExpandError {
    #[error("call `{call}`: unknown function `{func}`")]
    UnknownFunction { call: String, func: String },

    #[error("call `{call}` of `{func}`: missing required input `{input}`")]
    MissingInput {
        call: String,
        func: String,
        input: String,
    },

    #[error("call `{call}` of `{func}`: unknown input `{input}` (declared inputs: {declared})")]
    UnknownInput {
        call: String,
        func: String,
        input: String,
        declared: String,
    },

    #[error(
        "call `{call}` of `{func}`: input `{input}` is `{kind}`, so the argument must be a `@node` reference"
    )]
    NodeArgExpected {
        call: String,
        func: String,
        input: String,
        kind: &'static str,
    },

    #[error("function `{func}`: input `{input}` is `{kind}` — `default` is only allowed for scalar inputs")]
    NonScalarDefault {
        func: String,
        input: String,
        kind: &'static str,
    },

    #[error("function `{func}`: output node `{output}` is not in its body")]
    UnknownOutputNode { func: String, output: String },

    #[error("function `{func}`: `{name}` is both an input and a body node")]
    InputBodyCollision { func: String, name: String },

    #[error("function `{func}`, node `{node}`: unknown reference `@{reference}` (not an input, body node, or source)")]
    UnknownRef {
        func: String,
        node: String,
        reference: String,
    },

    #[error("recursive function call: {path}")]
    RecursiveCall { path: String },

    #[error("node id `{id}` contains `/`, which is reserved for expanded function bodies")]
    ReservedIdSeparator { id: String },

    #[error("function call `{call}`: missing `fn` field naming the function")]
    MissingFnName { call: String },

    #[error(
        "expansion produced more than {MAX_EXPANDED_NODES} nodes — check for heavily nested function calls"
    )]
    TooManyNodes,
}

/// A declared-kind check the host should verify once the graph is
/// built and port kinds are resolved. Produced per call site so kind
/// errors can name the call instead of an expanded internal port.
#[derive(Debug, Clone)]
pub struct KindCheck {
    /// Node id (in the expanded document) whose resolved output kind
    /// must match `declared`.
    pub node: String,
    pub declared: FuncKind,
    /// Call node id, for error messages.
    pub call: String,
    /// Function name, for error messages.
    pub func: String,
    /// Input name when this checks an argument; `None` for the
    /// function's own output kind.
    pub input: Option<String>,
}

/// Result of expanding a document's functions.
#[derive(Debug)]
pub struct Expanded {
    pub doc: Document,
    pub kind_checks: Vec<KindCheck>,
}

/// Expand every `op: "func"` call in `doc` into inline body copies.
/// Returns `None` when the document declares no functions (no work to
/// do — callers keep using the original document).
pub fn expand_functions(doc: &Document) -> Result<Option<Expanded>, ExpandError> {
    if doc.functions.is_empty() {
        return Ok(None);
    }

    // Reserved separator: user ids must not collide with mangled ones.
    for id in doc.nodes.keys() {
        if id.contains('/') {
            return Err(ExpandError::ReservedIdSeparator { id: id.clone() });
        }
    }
    for (fname, f) in &doc.functions {
        for id in f.nodes.keys() {
            if id.contains('/') {
                return Err(ExpandError::ReservedIdSeparator { id: id.clone() });
            }
            if f.inputs.contains_key(id) {
                return Err(ExpandError::InputBodyCollision {
                    func: fname.clone(),
                    name: id.clone(),
                });
            }
        }
        if !f.nodes.contains_key(f.output.as_str()) {
            return Err(ExpandError::UnknownOutputNode {
                func: fname.clone(),
                output: f.output.as_str().to_string(),
            });
        }
        for (iname, input) in &f.inputs {
            if input.default.is_some() && input.kind != FuncKind::Scalar {
                return Err(ExpandError::NonScalarDefault {
                    func: fname.clone(),
                    input: iname.clone(),
                    kind: input.kind.as_str(),
                });
            }
        }
    }

    check_call_cycles(&doc.functions)?;

    let mut cx = Expander {
        functions: &doc.functions,
        sources: &doc.sources,
        out: IndexMap::new(),
        kind_checks: Vec::new(),
    };

    for (id, spec) in &doc.nodes {
        if spec.op == "func" {
            cx.expand_call(id, &spec.fields)?;
        } else {
            cx.push(id.clone(), spec.clone())?;
        }
    }

    Ok(Some(Expanded {
        doc: Document {
            name: doc.name.clone(),
            version: doc.version.clone(),
            tile_size: doc.tile_size,
            pad: doc.pad,
            params: doc.params.clone(),
            attribution: doc.attribution.clone(),
            functions: IndexMap::new(),
            sources: doc.sources.clone(),
            nodes: cx.out,
            output: doc.output.clone(),
        },
        kind_checks: cx.kind_checks,
    }))
}

/// Reject cyclic function-to-function calls up front, with the path.
fn check_call_cycles(functions: &IndexMap<String, FuncDecl>) -> Result<(), ExpandError> {
    fn visit(
        name: &str,
        functions: &IndexMap<String, FuncDecl>,
        stack: &mut Vec<String>,
        done: &mut Vec<String>,
    ) -> Result<(), ExpandError> {
        if done.iter().any(|d| d == name) {
            return Ok(());
        }
        if let Some(pos) = stack.iter().position(|s| s == name) {
            let mut path: Vec<&str> = stack[pos..].iter().map(String::as_str).collect();
            path.push(name);
            return Err(ExpandError::RecursiveCall {
                path: path.join(" → "),
            });
        }
        let Some(f) = functions.get(name) else {
            // Unknown callee — reported with call context during
            // expansion, where the call node id is known.
            return Ok(());
        };
        stack.push(name.to_string());
        for spec in f.nodes.values() {
            if spec.op == "func" {
                if let Some(callee) = spec.fields.get("fn").and_then(Value::as_str) {
                    visit(callee, functions, stack, done)?;
                }
            }
        }
        stack.pop();
        done.push(name.to_string());
        Ok(())
    }

    let mut done = Vec::new();
    for name in functions.keys() {
        visit(name, functions, &mut Vec::new(), &mut done)?;
    }
    Ok(())
}

struct Expander<'a> {
    functions: &'a IndexMap<String, FuncDecl>,
    sources: &'a IndexMap<String, SourceDecl>,
    out: IndexMap<String, NodeSpec>,
    kind_checks: Vec<KindCheck>,
}

impl Expander<'_> {
    fn push(&mut self, id: String, spec: NodeSpec) -> Result<(), ExpandError> {
        if self.out.len() >= MAX_EXPANDED_NODES {
            return Err(ExpandError::TooManyNodes);
        }
        self.out.insert(id, spec);
        Ok(())
    }

    /// Expand one `op: "func"` call: validate the arguments, copy the
    /// body with mangled ids and substituted inputs, recurse into
    /// nested calls. The body's output node is inserted under the call
    /// id itself so outer `@call` references resolve unchanged.
    fn expand_call(
        &mut self,
        call_id: &str,
        fields: &serde_json::Map<String, Value>,
    ) -> Result<(), ExpandError> {
        let func_name =
            fields
                .get("fn")
                .and_then(Value::as_str)
                .ok_or_else(|| ExpandError::MissingFnName {
                    call: call_id.to_string(),
                })?;
        let func = self
            .functions
            .get(func_name)
            .ok_or_else(|| ExpandError::UnknownFunction {
                call: call_id.to_string(),
                func: func_name.to_string(),
            })?;

        // Validate the argument set against the declared inputs.
        for key in fields.keys() {
            if key == "op" || key == "fn" {
                continue;
            }
            if !func.inputs.contains_key(key) {
                return Err(ExpandError::UnknownInput {
                    call: call_id.to_string(),
                    func: func_name.to_string(),
                    input: key.clone(),
                    declared: func
                        .inputs
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", "),
                });
            }
        }

        // Build the substitution map: input name -> argument value.
        let mut subst: IndexMap<String, Value> = IndexMap::new();
        for (iname, input) in &func.inputs {
            let arg = match fields.get(iname) {
                Some(v) => v.clone(),
                None => match &input.default {
                    Some(d) => d.clone(),
                    None => {
                        return Err(ExpandError::MissingInput {
                            call: call_id.to_string(),
                            func: func_name.to_string(),
                            input: iname.clone(),
                        });
                    }
                },
            };
            let arg_node = arg
                .as_str()
                .and_then(|s| s.strip_prefix('@'))
                .map(str::to_string);
            if input.kind != FuncKind::Scalar && arg_node.is_none() {
                return Err(ExpandError::NodeArgExpected {
                    call: call_id.to_string(),
                    func: func_name.to_string(),
                    input: iname.clone(),
                    kind: input.kind.as_str(),
                });
            }
            // Node-fed arguments get their resolved kind verified once
            // the graph is built (scalar literals/params need no check
            // — the In<T> readers enforce value types).
            if let Some(src) = arg_node {
                self.kind_checks.push(KindCheck {
                    node: src,
                    declared: input.kind,
                    call: call_id.to_string(),
                    func: func_name.to_string(),
                    input: Some(iname.clone()),
                });
            }
            subst.insert(iname.clone(), arg);
        }

        // Mangle body ids; the output node takes the call id.
        let output_id = func.output.as_str();
        let mangled = |body_id: &str| -> String {
            if body_id == output_id {
                call_id.to_string()
            } else {
                format!("{call_id}/{body_id}")
            }
        };

        self.kind_checks.push(KindCheck {
            node: call_id.to_string(),
            declared: func.output_kind,
            call: call_id.to_string(),
            func: func_name.to_string(),
            input: None,
        });

        for (body_id, spec) in &func.nodes {
            let new_id = mangled(body_id);
            let mut new_fields = serde_json::Map::with_capacity(spec.fields.len());
            for (k, v) in &spec.fields {
                // A field that IS an input reference whose argument is
                // `null` disappears from the node — the way to leave
                // optional op fields (stroke curves, seeds, …) unset
                // from a call site.
                if let Some(name) = v.as_str().and_then(|s| s.strip_prefix('@')) {
                    if subst.get(name) == Some(&Value::Null) {
                        continue;
                    }
                }
                new_fields.insert(
                    k.clone(),
                    self.rewrite(v, &subst, func, func_name, body_id, &mangled)?,
                );
            }
            if spec.op == "func" {
                self.expand_call(&new_id, &new_fields)?;
            } else {
                self.push(
                    new_id,
                    NodeSpec {
                        op: spec.op.clone(),
                        fields: new_fields,
                    },
                )?;
            }
        }
        Ok(())
    }

    /// Rewrite one body field value: substitute `@input` references
    /// with argument values, remap `@body-node` references to mangled
    /// ids, let `@source` references pass through, and reject anything
    /// else. Recurses into arrays and objects so references inside
    /// e.g. gradient stops are covered.
    fn rewrite(
        &self,
        v: &Value,
        subst: &IndexMap<String, Value>,
        func: &FuncDecl,
        func_name: &str,
        body_id: &str,
        mangled: &dyn Fn(&str) -> String,
    ) -> Result<Value, ExpandError> {
        match v {
            Value::String(s) => {
                let Some(name) = s.strip_prefix('@') else {
                    return Ok(v.clone());
                };
                if let Some(arg) = subst.get(name) {
                    return Ok(arg.clone());
                }
                if func.nodes.contains_key(name) {
                    return Ok(Value::String(format!("@{}", mangled(name))));
                }
                if self.sources.contains_key(name) {
                    return Ok(v.clone());
                }
                Err(ExpandError::UnknownRef {
                    func: func_name.to_string(),
                    node: body_id.to_string(),
                    reference: name.to_string(),
                })
            }
            Value::Array(items) => Ok(Value::Array(
                items
                    .iter()
                    .map(|item| self.rewrite(item, subst, func, func_name, body_id, mangled))
                    .collect::<Result<_, _>>()?,
            )),
            Value::Object(map) => {
                let mut out = serde_json::Map::with_capacity(map.len());
                for (k, item) in map {
                    out.insert(
                        k.clone(),
                        self.rewrite(item, subst, func, func_name, body_id, mangled)?,
                    );
                }
                Ok(Value::Object(out))
            }
            _ => Ok(v.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expand(json: &str) -> Result<Option<Expanded>, ExpandError> {
        let doc = Document::from_json(json).unwrap();
        expand_functions(&doc)
    }

    fn expanded(json: &str) -> Document {
        expand(json).unwrap().expect("functions present").doc
    }

    const BASIC: &str = r##"{
      "name": "demo",
      "functions": {
        "tinted": {
          "inputs": {
            "base":  { "kind": "raster" },
            "color": { "kind": "scalar", "default": "#ff0000" }
          },
          "output": "@mix",
          "output-kind": "raster",
          "nodes": {
            "tint": { "op": "solid", "color": "@color" },
            "mix":  { "op": "blend", "base": "@base", "over": "@tint" }
          }
        }
      },
      "nodes": {
        "bg": { "op": "solid", "color": "#ffffff" },
        "out": { "op": "func", "fn": "tinted", "base": "@bg", "color": "#00ff00" }
      },
      "output": "@out"
    }"##;

    #[test]
    fn expands_with_mangling_and_output_alias() {
        let doc = expanded(BASIC);
        assert!(doc.functions.is_empty());
        let ids: Vec<&str> = doc.nodes.keys().map(String::as_str).collect();
        assert_eq!(ids, ["bg", "out/tint", "out"]);
        // Output body node took the call id; its internal `@tint` ref
        // was mangled; `@base` was substituted with the caller arg.
        assert_eq!(doc.nodes["out"].fields["base"], "@bg");
        assert_eq!(doc.nodes["out"].fields["over"], "@out/tint");
        // The scalar arg substituted as a literal.
        assert_eq!(doc.nodes["out/tint"].fields["color"], "#00ff00");
    }

    #[test]
    fn default_fills_missing_scalar_arg() {
        let json = BASIC.replace(r##", "color": "#00ff00""##, "");
        let doc = expanded(&json);
        assert_eq!(doc.nodes["out/tint"].fields["color"], "#ff0000");
    }

    #[test]
    fn param_args_stay_params() {
        let json = BASIC.replace(r##""color": "#00ff00""##, r##""color": "$ink""##);
        let doc = expanded(&json);
        assert_eq!(doc.nodes["out/tint"].fields["color"], "$ink");
    }

    #[test]
    fn missing_required_input_errors() {
        let json = BASIC.replace(r#", "base": "@bg""#, "");
        let err = expand(&json).unwrap_err();
        assert!(matches!(err, ExpandError::MissingInput { input, .. } if input == "base"));
    }

    #[test]
    fn unknown_input_errors() {
        let json = BASIC.replace(r##""color": "#00ff00""##, r##""colour": "#00ff00""##);
        let err = expand(&json).unwrap_err();
        assert!(matches!(err, ExpandError::UnknownInput { input, .. } if input == "colour"));
    }

    #[test]
    fn non_scalar_input_requires_node_arg() {
        let json = BASIC.replace(r#""base": "@bg""#, r#""base": 3"#);
        let err = expand(&json).unwrap_err();
        assert!(matches!(err, ExpandError::NodeArgExpected { input, .. } if input == "base"));
    }

    #[test]
    fn unknown_body_ref_errors() {
        let json = BASIC.replace(r#""base": "@base""#, r#""base": "@nope""#);
        let err = expand(&json).unwrap_err();
        assert!(matches!(err, ExpandError::UnknownRef { reference, .. } if reference == "nope"));
    }

    #[test]
    fn nested_function_calls_expand() {
        let json = r##"{
          "name": "demo",
          "functions": {
            "white": {
              "inputs": {},
              "output": "@w",
              "output-kind": "raster",
              "nodes": { "w": { "op": "solid", "color": "#ffffff" } }
            },
            "framed": {
              "inputs": {},
              "output": "@mix",
              "output-kind": "raster",
              "nodes": {
                "fill": { "op": "func", "fn": "white" },
                "mix":  { "op": "blend", "base": "@fill", "over": "@fill" }
              }
            }
          },
          "nodes": { "out": { "op": "func", "fn": "framed" } },
          "output": "@out"
        }"##;
        let doc = expanded(json);
        let ids: Vec<&str> = doc.nodes.keys().map(String::as_str).collect();
        // `fill` is itself a call: its body's output node takes the
        // (mangled) call id `out/fill`.
        assert_eq!(ids, ["out/fill", "out"]);
        assert_eq!(doc.nodes["out"].fields["base"], "@out/fill");
    }

    #[test]
    fn recursive_calls_error_with_path() {
        let json = r##"{
          "name": "demo",
          "functions": {
            "a": { "inputs": {}, "output": "@n", "output-kind": "raster",
                   "nodes": { "n": { "op": "func", "fn": "b" } } },
            "b": { "inputs": {}, "output": "@n", "output-kind": "raster",
                   "nodes": { "n": { "op": "func", "fn": "a" } } }
          },
          "nodes": { "out": { "op": "func", "fn": "a" } },
          "output": "@out"
        }"##;
        let err = expand(json).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("a → b → a") || msg.contains("b → a → b"),
            "{msg}"
        );
    }

    #[test]
    fn no_functions_is_a_noop() {
        let json = r##"{
          "name": "demo",
          "nodes": { "out": { "op": "solid", "color": "#ffffff" } },
          "output": "@out"
        }"##;
        assert!(expand(json).unwrap().is_none());
    }

    #[test]
    fn substitution_recurses_into_arrays() {
        let json = r##"{
          "name": "demo",
          "functions": {
            "ramp": {
              "inputs": { "lo": { "kind": "scalar", "default": "#000000" } },
              "output": "@g",
              "output-kind": "raster",
              "nodes": {
                "g": { "op": "gradient-linear",
                       "stops": [[0, "@lo"], [1, "#ffffff"]] }
              }
            }
          },
          "nodes": { "out": { "op": "func", "fn": "ramp", "lo": "#101010" } },
          "output": "@out"
        }"##;
        let doc = expanded(json);
        assert_eq!(doc.nodes["out"].fields["stops"][0][1], "#101010");
    }

    #[test]
    fn null_arg_drops_the_substituted_field() {
        let json = r##"{
          "name": "demo",
          "functions": {
            "stroke": {
              "inputs": {
                "curve": { "kind": "scalar", "default": null }
              },
              "output": "@n",
              "output-kind": "raster",
              "nodes": {
                "n": { "op": "solid", "color": "#ffffff",
                       "radius-stroke-curve": "@curve" }
              }
            }
          },
          "nodes": {
            "a": { "op": "func", "fn": "stroke" },
            "b": { "op": "func", "fn": "stroke", "curve": [[0, -1.0], [1, 0.0]] }
          },
          "output": "@a"
        }"##;
        let doc = expanded(json);
        // Omitted arg -> null default -> field dropped entirely.
        assert!(!doc.nodes["a"].fields.contains_key("radius-stroke-curve"));
        // Array arg substitutes verbatim.
        assert_eq!(doc.nodes["b"].fields["radius-stroke-curve"][0][1], -1.0);
    }

    #[test]
    fn kind_checks_record_call_sites() {
        let e = expand(BASIC).unwrap().unwrap();
        // One check for the node-fed `base` arg, one for the output.
        assert_eq!(e.kind_checks.len(), 2);
        let arg = &e.kind_checks[0];
        assert_eq!(arg.node, "bg");
        assert_eq!(arg.declared, FuncKind::Raster);
        assert_eq!(arg.input.as_deref(), Some("base"));
        let out = &e.kind_checks[1];
        assert_eq!(out.node, "out");
        assert!(out.input.is_none());
    }
}
