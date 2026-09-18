//! Native tests.
//!
//! The ABI itself cannot be driven from here: its addresses are `u32`
//! offsets into a wasm linear memory, and a native pointer does not fit in
//! one. So what a native test can hold is everything that is not pointer
//! arithmetic — the registry count the module is checked against, the
//! option parsing and the error kinds it produces, and whether the
//! committed module still carries the surface this file declares. Driving
//! the ABI end to end is the Go package's job, where there is a real linear
//! memory to put the arguments in.

use super::*;

/// The number the wasm module is compared against: if the two disagree, the
/// module's life-before-main constructors did not all run. `go/ezu_test.go`
/// keeps the same constant.
#[test]
fn native_op_count() {
    let n = op_count();
    println!("native op_count = {n}");
    assert!(n > 0, "the node registry is empty natively, which is a bug");
}

#[test]
fn every_error_kind_has_its_own_code() {
    let kinds = [
        ErrorKind::InvalidStyle,
        ErrorKind::BrushParse,
        ErrorKind::MvtDecode,
        ErrorKind::DemDecode,
        ErrorKind::RasterDecode,
        ErrorKind::GeoJsonDecode,
        ErrorKind::SpriteDecode,
        ErrorKind::FontParse,
        ErrorKind::GlyphDecode,
        ErrorKind::RenderFailed,
        ErrorKind::PngEncode,
        ErrorKind::WebpEncode,
        ErrorKind::UnknownSource,
        ErrorKind::OutOfMemory,
    ];
    let mut codes: Vec<i64> = kinds.iter().copied().map(code_of).collect();
    let before = codes.len();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), before, "two kinds share a code");
    assert!(
        codes.iter().all(|c| *c < 0 && *c > ERR_BAD_HANDLE),
        "a kind's code collides with the shell's own reserved range"
    );
}

mod options {
    use crate::options::{parse_bind, parse_render};
    use ezu_renderer::{OutputFormat, PngCompression};

    #[test]
    fn an_empty_payload_is_the_default() {
        let b = parse_bind(b"").expect("empty is {}");
        assert_eq!(b.coord, (0, 0));
        assert_eq!(b.source_zoom, None);
        assert!(b.index.is_none());
        let r = parse_render(b"").expect("empty is {}");
        assert_eq!(r.format, OutputFormat::Png);
        assert_eq!(r.tile_size, None);
        assert_eq!(r.pad, None);
        assert!(r.params.is_empty());
    }

    #[test]
    fn bind_options_are_read_the_way_the_js_shell_reads_them() {
        let b = parse_bind(br#"{"coord":[-1,1],"sourceZoom":14,"index":"{}"}"#).expect("parses");
        assert_eq!(b.coord, (-1, 1));
        assert_eq!(b.source_zoom, Some(14));
        assert_eq!(b.index.as_deref(), Some("{}"));
        // An unknown key is a field from a newer host, not a mistake.
        assert_eq!(parse_bind(br#"{"whatIsThis":1}"#).unwrap().coord, (0, 0));
    }

    /// The names are the contract, and these are the ones the JS shell
    /// raises for the same malformed options.
    #[test]
    fn refused_bind_options_carry_the_js_shells_names() {
        for payload in [
            &br#"{"coord":[2,0]}"#[..],
            &br#"{"coord":[0]}"#[..],
            &br#"{"coord":"nope"}"#[..],
            &br#"{"sourceZoom":99}"#[..],
            &br#"{"sourceZoom":"x"}"#[..],
        ] {
            let e = parse_bind(payload).expect_err("refused");
            assert_eq!(
                e.name(),
                "UnknownSource",
                "for {}",
                String::from_utf8_lossy(payload)
            );
        }
    }

    #[test]
    fn render_options_are_read_the_way_the_js_shell_reads_them() {
        let r = parse_render(
            br#"{"format":"webp","tileSize":256,"pad":8,
                 "png":{"compression":"best"},
                 "params":{"a":1.5,"b":true,"c":"x"}}"#,
        )
        .expect("parses");
        assert_eq!(r.format, OutputFormat::Webp);
        assert_eq!(r.tile_size, Some(256));
        assert_eq!(r.pad, Some(8));
        assert_eq!(r.png_compression, PngCompression::Best);
        let mut params = r.params.clone();
        params.sort();
        assert_eq!(
            params,
            vec![
                ("a".to_string(), "1.5".to_string()),
                ("b".to_string(), "true".to_string()),
                ("c".to_string(), "x".to_string()),
            ],
            "every param crosses as text, so the renderer's own parser owns the coercions"
        );
    }

    /// An unrecognised format is a typo rather than a request for the
    /// default — the JS shell says so too, with the same kind.
    #[test]
    fn refused_render_options_carry_the_js_shells_names() {
        for payload in [
            &br#"{"format":"jpeg"}"#[..],
            &br#"{"png":{"compression":"maximum"}}"#[..],
            &br#"{"tileSize":-4}"#[..],
            &br#"{not json"#[..],
            &br#"[1,2]"#[..],
        ] {
            let e = parse_render(payload).expect_err("refused");
            assert_eq!(
                e.name(),
                "InvalidStyle",
                "for {}",
                String::from_utf8_lossy(payload)
            );
        }
    }
}

/// Does the committed `go/ezu_cabi.wasm` still carry the surface this
/// source declares?
///
/// Structural rather than byte-for-byte on purpose: Rust builds are not
/// reproducible across environments, and a check that cried wolf on every
/// toolchain bump would teach people to ignore it. An export list does not
/// care which toolchain produced it, and an export appearing or leaving is
/// exactly the drift worth catching — together with the ABI version the Go
/// package checks at instantiation, and the CI job that runs the Go tests a
/// second time against a freshly built module.
mod committed_module {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn module_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../go/ezu_cabi.wasm")
            .canonicalize()
            .expect("go/ezu_cabi.wasm is committed alongside the Go package")
    }

    struct Surface {
        functions: BTreeSet<String>,
        imports: BTreeSet<String>,
    }

    fn surface() -> Surface {
        let bytes = std::fs::read(module_path()).expect("reading the committed module");
        let mut functions = BTreeSet::new();
        let mut imports = BTreeSet::new();
        for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
            match payload.expect("the committed module is valid wasm") {
                wasmparser::Payload::ExportSection(section) => {
                    for export in section {
                        let export = export.expect("a readable export");
                        if export.kind == wasmparser::ExternalKind::Func {
                            functions.insert(export.name.to_string());
                        }
                    }
                }
                wasmparser::Payload::ImportSection(section) => {
                    for import in section {
                        let import = import.expect("a readable import");
                        imports.insert(format!("{}.{}", import.module, import.name));
                    }
                }
                _ => {}
            }
        }
        Surface { functions, imports }
    }

    #[test]
    fn the_committed_module_exports_exactly_this_surface() {
        let got = surface().functions;
        let declared: BTreeSet<String> = crate::EXPORTS.iter().map(|s| s.to_string()).collect();
        let present: BTreeSet<String> = got
            .iter()
            .filter(|n| n.starts_with("ezu_"))
            .cloned()
            .collect();
        assert_eq!(
            present, declared,
            "the committed module's ezu_* exports differ from EXPORTS: rebuild it with \
             `go generate ./...`"
        );
    }

    /// The linkage shape, read off the artefact rather than trusted. Both
    /// entry points into the life-before-main constructors must be there:
    /// `_initialize` for a host that runs it on instantiation, and
    /// `__wasm_call_ctors` for one that does not. `crates/ezu-cabi/build.rs`
    /// says why the shape is asked for rather than inherited.
    #[test]
    fn the_committed_module_is_reactor_shaped() {
        let got = surface().functions;
        for entry in ["_initialize", "__wasm_call_ctors"] {
            assert!(
                got.contains(entry),
                "the committed module does not export {entry}, so it fell back to \
                 command-style linkage"
            );
        }
    }

    /// The allocator's escape hatch is an import, so a host that does not
    /// provide it cannot instantiate the module at all — which is the point
    /// (see `src/oom.rs`). If this import ever disappears, heap exhaustion
    /// goes back to being a bare `unreachable`.
    #[test]
    fn the_committed_module_asks_the_host_about_a_failed_allocation() {
        assert!(
            surface().imports.contains("ezu_host.oom"),
            "the committed module does not import ezu_host.oom"
        );
    }
}
