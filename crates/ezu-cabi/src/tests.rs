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
    use crate::options::{parse_bind, parse_frame, parse_render, parse_set};
    use ezu::renderer::{OutputFormat, PngCompression};

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
    fn a_source_set_is_read_entry_by_entry() {
        assert!(parse_set(b"").expect("empty is []").is_empty());
        assert!(parse_set(b"null").expect("null is []").is_empty());
        assert!(parse_set(b"[]").expect("parses").is_empty());

        let set = parse_set(
            br#"[{"data":7},
                 {"data":8,"coord":[1,-1]},
                 {"data":3,"coord":[0,1],"sourceZoom":13,"newerField":true}]"#,
        )
        .expect("parses");
        let got: Vec<_> = set
            .iter()
            .map(|e| (e.data, e.opts.coord, e.opts.source_zoom))
            .collect();
        assert_eq!(
            got,
            [(7, (0, 0), None), (8, (1, -1), None), (3, (0, 1), Some(13))]
        );
        assert!(set.iter().all(|e| e.opts.index.is_none()));
    }

    /// An entry is refused the way the bind options are: a bad place is
    /// `UnknownSource`, and so is an entry that names no loaded data. A
    /// payload that is not a list of objects is malformed, `InvalidStyle`.
    #[test]
    fn refused_source_sets_carry_the_bind_options_names() {
        for payload in [
            &br#"[{"data":1,"coord":[2,0]}]"#[..],
            &br#"[{"data":1,"sourceZoom":99}]"#[..],
            &br#"[{"coord":[0,0]}]"#[..],
            &br#"[{"data":0}]"#[..],
            &br#"[{"data":-3}]"#[..],
            &br#"[{"data":"7"}]"#[..],
        ] {
            let e = parse_set(payload).expect_err("refused");
            assert_eq!(
                e.name(),
                "UnknownSource",
                "for {}",
                String::from_utf8_lossy(payload)
            );
        }
        for payload in [&br#"{"data":1}"#[..], &br#"[1]"#[..], &br#"[{"#[..]] {
            let e = parse_set(payload).expect_err("refused");
            assert_eq!(
                e.name(),
                "InvalidStyle",
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

    #[test]
    fn a_frame_request_takes_the_style_tile_size_unless_it_names_one() {
        let req = parse_frame(
            br#"{"centerLon":139.7,"centerLat":35.68,"scale":1000,"dpi":200,
                 "width":1929,"height":1370}"#,
            512,
        )
        .expect("parses");
        assert_eq!((req.center_lon, req.center_lat), (139.7, 35.68));
        assert_eq!((req.scale, req.dpi), (1000.0, 200.0));
        assert_eq!((req.width, req.height), (1929, 1370));
        assert_eq!(req.zoom, None);
        assert_eq!(req.style_tile_size, 512);

        let req = parse_frame(
            br#"{"centerLon":0,"centerLat":0,"scale":1000,"dpi":96,
                 "width":10,"height":10,"zoom":18,"styleTileSize":256}"#,
            512,
        )
        .expect("parses");
        assert_eq!(req.zoom, Some(18));
        assert_eq!(req.style_tile_size, 256);
    }

    #[test]
    fn a_frame_request_without_a_scale_or_a_size_is_refused() {
        for payload in [
            &br#"{}"#[..],
            &br#"{"centerLon":0,"centerLat":0,"dpi":200,"width":1,"height":1}"#[..],
            &br#"{"centerLon":0,"centerLat":0,"scale":1000,"dpi":200,"width":1}"#[..],
            &br#"{"centerLon":0,"centerLat":0,"scale":1000,"dpi":200,
                  "width":1,"height":1,"zoom":-1}"#[..],
        ] {
            let e = parse_frame(payload, 512).expect_err("refused");
            assert_eq!(
                e.name(),
                "InvalidStyle",
                "for {}",
                String::from_utf8_lossy(payload)
            );
        }
    }
}

/// Loaded data belongs to the renderer that loaded it. None of this needs
/// a pointer, so it can be held natively: a renderer goes into the table
/// the way `ezu_renderer_new` puts it there, and the data the way
/// `ezu_load_source` files it.
mod loaded_sources {
    use super::*;

    const STAINED_GLASS: &str = include_str!("../../ezu/examples/styles/stained-glass.json");
    const BASEMAP_Z13: &[u8] = include_bytes!("../../../go/testdata/basemap-13-7277-3227.mvt");

    fn renderer() -> u32 {
        let handle = NEXT_HANDLE.with(|n| {
            let mut n = n.borrow_mut();
            let h = *n;
            *n += 1;
            h
        });
        let r = Renderer::new(STAINED_GLASS).expect("builds");
        RENDERERS.with(|t| t.borrow_mut().insert(handle, r));
        handle
    }

    fn load(handle: u32) -> u32 {
        let data = RENDERERS.with(|t| {
            t.borrow()[&handle]
                .load_source("basemap", BASEMAP_Z13)
                .expect("loads")
        });
        keep_source(handle, data)
    }

    fn last_error() -> (&'static str, String) {
        LAST_ERROR.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
    }

    #[test]
    fn a_set_names_only_data_its_renderer_holds() {
        let (a, b) = (renderer(), renderer());
        let id = load(a);
        let set = format!(r#"[{{"data":{id}}},{{"data":{id},"coord":[1,0],"sourceZoom":13}}]"#);

        let built = source_set(a, set.as_bytes()).expect("builds");
        assert!(built.contains("basemap"));

        let unknown = code_of(ErrorKind::UnknownSource);
        assert_eq!(source_set(b, set.as_bytes()).err(), Some(unknown));
        assert_eq!(last_error().0, "UnknownSource");
        assert_eq!(ezu_release_source(b, id), unknown, "b did not load it");

        assert_eq!(ezu_release_source(a, id), 0);
        assert_eq!(ezu_release_source(a, id), unknown, "released twice");
        assert_eq!(source_set(a, set.as_bytes()).err(), Some(unknown));
        let (name, message) = last_error();
        assert_eq!(name, "UnknownSource");
        assert!(message.contains(&id.to_string()), "{message}");

        // Freeing a renderer drops what it loaded, and nothing else.
        let (kept, dropped) = (load(b), load(a));
        assert_eq!(ezu_renderer_free(a), 0);
        assert!(SOURCES.with(|s| !s.borrow().contains_key(&dropped)));
        assert!(SOURCES.with(|s| s.borrow().contains_key(&kept)));
        assert_eq!(ezu_renderer_free(b), 0);
    }

    #[test]
    fn a_set_with_no_entries_renders_no_tile_data() {
        let a = renderer();
        assert!(source_set(a, b"").expect("builds").is_empty());
        assert_eq!(ezu_trim_glyphs(a), 0);
        assert_eq!(ezu_renderer_free(a), 0);
        assert_eq!(ezu_trim_glyphs(a), ERR_BAD_HANDLE);
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
