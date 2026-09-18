//! A flat C ABI over the ezu renderer, for hosts that embed the module as
//! `wasm32-wasip1` and drive it over linear memory (wazero, wasmtime, …).
//!
//! This is the second of ezu's embedding shells. The first is
//! `ezu-wasm`, the wasm-bindgen module a browser loads; both are thin
//! translation layers over the same [`ezu_renderer::Renderer`], and both
//! expose the same surface. What lives here is the translation: reading
//! options out of JSON, writing answers back into linear memory, turning
//! an [`ezu_renderer::Error`] into a code plus a name the host branches
//! on, reporting a failed allocation, and buffering `tracing` events for
//! the host to drain.
//!
//! # Conventions
//!
//! - Every buffer the host hands in is a `(ptr, len)` pair into linear
//!   memory that the host obtained from [`ezu_alloc`] and still owns.
//! - Every buffer the module hands back is written into two *out-slots* —
//!   a `(ptr, len)` pair the host allocated, four bytes each, little-endian
//!   — and the buffer becomes the host's to [`ezu_free`].
//! - Non-negative returns are results. Negative returns are error codes,
//!   with the detail in [`ezu_last_error`].
//! - A renderer is a handle: a non-zero `u32` index into a module-global
//!   table. Handles are never reused.
//! - Anything richer than a number or a byte string crosses as JSON text:
//!   option objects in, schema/legend/usage documents out. The field names
//!   are the ones `ezu-wasm` uses, so the two shells are readable against
//!   each other.
//!
//! # Errors
//!
//! [`ezu_last_error`] answers with `"<Name>\n<message>"`: the first line is
//! [`ezu_renderer::ErrorKind::name`], the rest is the human-readable
//! detail. The names are a public contract shared with the JS shell, where
//! they arrive as a thrown `Error`'s `.name` — a Go caller branches on the
//! same string a browser caller does. The numeric code is a cheaper copy of
//! the same discrimination ([`code_of`]); the name is the authoritative one.
//!
//! # Threading
//!
//! A module instance is single-threaded and its allocator is not safe for
//! concurrent entry. Everything below is `thread_local!` for that reason,
//! and a host that wants to render two tiles at once instantiates the
//! module twice.

mod log;
mod oom;
mod options;

use std::cell::RefCell;
use std::collections::HashMap;

use ezu_renderer::{Error, ErrorKind, Renderer};

/// Incremented whenever the surface changes — a signature, a format, a
/// code's meaning, or an export appearing or leaving. The Go package
/// checks it at instantiation, which is what catches a committed module
/// that was not rebuilt.
pub const ABI_VERSION: u32 = 1;

/// Every function this shell exports, in one list, so a test can compare
/// it with what the committed module actually carries. The linker's own
/// exports (`memory`, `_initialize`, `__wasm_call_ctors`, …) are not here;
/// the test names those separately, since they come from the linkage shape
/// rather than from this file.
pub const EXPORTS: &[&str] = &[
    "ezu_abi_version",
    "ezu_alloc",
    "ezu_free",
    "ezu_last_error",
    "ezu_op_count",
    "ezu_op_names",
    "ezu_simd_enabled",
    "ezu_heap_bytes",
    "ezu_log_init",
    "ezu_drain_logs",
    "ezu_renderer_new",
    "ezu_renderer_free",
    "ezu_set_style",
    "ezu_tile_size",
    "ezu_params_schema",
    "ezu_attribution",
    "ezu_legend",
    "ezu_bind_source",
    "ezu_clear_sources",
    "ezu_bound_sources",
    "ezu_set_glyph_budget",
    "ezu_source_tile",
    "ezu_requested_neighbor_offsets",
    "ezu_needed_codepoints",
    "ezu_needed_glyph_ranges",
    "ezu_memory_usage",
    "ezu_render_tile",
];

/// The negative return for a failure of `kind`.
///
/// A flat ABI has to say "this failed" in the return value, and a distinct
/// number per kind costs nothing over a single sentinel. It is a
/// convenience, not the contract: the name in [`ezu_last_error`] is, and a
/// host that only reads the code will not see kinds added later.
pub const fn code_of(kind: ErrorKind) -> i64 {
    match kind {
        ErrorKind::InvalidStyle => -1,
        ErrorKind::BrushParse => -2,
        ErrorKind::MvtDecode => -3,
        ErrorKind::DemDecode => -4,
        ErrorKind::RasterDecode => -5,
        ErrorKind::GeoJsonDecode => -6,
        ErrorKind::SpriteDecode => -7,
        ErrorKind::FontParse => -8,
        ErrorKind::GlyphDecode => -9,
        ErrorKind::RenderFailed => -10,
        ErrorKind::PngEncode => -11,
        ErrorKind::WebpEncode => -12,
        ErrorKind::UnknownSource => -13,
        ErrorKind::OutOfMemory => -14,
    }
}

/// The name this shell reports when a call names a handle that is not
/// live. It has no counterpart in the JS shell, where the renderer is a
/// JS object and there is nothing to get wrong.
pub const BAD_HANDLE: &str = "BadHandle";

/// The negative return for [`BAD_HANDLE`]. Kept well clear of
/// [`code_of`]'s range so adding an [`ErrorKind`] cannot collide with it.
pub const ERR_BAD_HANDLE: i64 = -100;

// A module instance is single-threaded, so thread-locals are the whole
// story: no locks, and no `Send + Sync` bound on anything a renderer holds.
thread_local! {
    /// `(name, message)` of the most recent failure.
    static LAST_ERROR: RefCell<(&'static str, String)> =
        const { RefCell::new(("", String::new())) };
    static RENDERERS: RefCell<HashMap<u32, Renderer>> = RefCell::new(HashMap::new());
    static NEXT_HANDLE: RefCell<u32> = const { RefCell::new(1) };
}

/// Record a failure and return its code.
fn fail(kind: ErrorKind, message: impl std::fmt::Display) -> i64 {
    LAST_ERROR.with(|slot| *slot.borrow_mut() = (kind.name(), message.to_string()));
    code_of(kind)
}

fn fail_err(e: Error) -> i64 {
    fail(e.kind(), e)
}

fn bad_handle(handle: u32) -> i64 {
    LAST_ERROR.with(|slot| {
        *slot.borrow_mut() = (BAD_HANDLE, format!("no renderer with handle {handle}"))
    });
    ERR_BAD_HANDLE
}

/// Hand a buffer to the host: transfer ownership and write its location
/// into the two out-slots. Unaligned, because the slots live wherever the
/// host put them — typically inside an `ezu_alloc` buffer, which promises
/// no alignment.
///
/// # Safety
/// `out_ptr` and `out_len` must each address four writable bytes.
unsafe fn reply(out_ptr: u32, out_len: u32, bytes: Vec<u8>) {
    let boxed = bytes.into_boxed_slice();
    let len = boxed.len() as u32;
    let ptr = Box::into_raw(boxed) as *mut u8 as u32;
    unsafe {
        (out_ptr as *mut u32).write_unaligned(ptr);
        (out_len as *mut u32).write_unaligned(len);
    }
}

/// # Safety
/// `ptr` must address `len` readable bytes for the duration of the call.
unsafe fn input<'a>(ptr: u32, len: u32) -> &'a [u8] {
    if len == 0 {
        return &[];
    }
    unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) }
}

/// Read a host-supplied UTF-8 string, or record why it could not be read.
///
/// # Safety
/// As [`input`].
unsafe fn input_str<'a>(ptr: u32, len: u32, what: &str) -> Result<&'a str, i64> {
    std::str::from_utf8(unsafe { input(ptr, len) })
        .map_err(|_| fail(ErrorKind::InvalidStyle, format!("the {what} is not utf-8")))
}

/// Run `f` against the renderer behind `handle`, or report a bad handle.
fn with_renderer<F: FnOnce(&Renderer) -> i64>(handle: u32, f: F) -> i64 {
    RENDERERS.with(|r| match r.borrow().get(&handle) {
        Some(renderer) => f(renderer),
        None => bad_handle(handle),
    })
}

fn with_renderer_mut<F: FnOnce(&mut Renderer) -> i64>(handle: u32, f: F) -> i64 {
    RENDERERS.with(|r| match r.borrow_mut().get_mut(&handle) {
        Some(renderer) => f(renderer),
        None => bad_handle(handle),
    })
}

/// Write `json` (already serialised) into the out-slots and return 0.
///
/// # Safety
/// As [`reply`].
unsafe fn reply_json(out_ptr: u32, out_len: u32, json: String) -> i64 {
    unsafe { reply(out_ptr, out_len, json.into_bytes()) };
    0
}

// --- lifecycle -------------------------------------------------------------

/// The standard reactor entry point: run the life-before-main constructors,
/// which is where `inventory::submit!` registers every node op.
///
/// `build.rs` pins this module to the reactor shape, so nothing runs the
/// constructors on its own and a host that skips this gets an empty
/// registry — `ezu_op_count() == 0` and "unknown op" for every style. A
/// WASI host calls `_initialize` on instantiation; one that does not may
/// call the exported `__wasm_call_ctors` instead. Either, or both, is fine:
/// `inventory` guards each submission with an `AtomicBool` on wasm, so
/// running the constructors twice registers nothing twice.
#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub extern "C" fn _initialize() {
    unsafe extern "C" {
        fn __wasm_call_ctors();
    }
    unsafe { __wasm_call_ctors() };
}

#[unsafe(no_mangle)]
pub extern "C" fn ezu_abi_version() -> u32 {
    ABI_VERSION
}

/// Reserve `len` bytes of the module's heap and return their address. The
/// host owns the buffer until it passes the same `(ptr, len)` to
/// [`ezu_free`].
#[unsafe(no_mangle)]
pub extern "C" fn ezu_alloc(len: u32) -> u32 {
    let boxed = vec![0u8; len as usize].into_boxed_slice();
    Box::into_raw(boxed) as *mut u8 as u32
}

/// Release a buffer obtained from [`ezu_alloc`] or handed back in an
/// out-slot. `len` must be the length that came with it.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_free(ptr: u32, len: u32) {
    if ptr == 0 {
        return;
    }
    unsafe {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            ptr as *mut u8,
            len as usize,
        )));
    }
}

/// The most recent failure as `"<Name>\n<message>"`, cleared by reading.
///
/// The first line is [`ezu_renderer::ErrorKind::name`] — the same string
/// the JS shell puts on a thrown `Error`'s `.name` — or [`BAD_HANDLE`].
/// The remainder is the detail, which may itself contain newlines. An
/// empty buffer means nothing has failed since the last read.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_last_error(out_ptr: u32, out_len: u32) {
    let (name, message) = LAST_ERROR.with(|slot| {
        let mut slot = slot.borrow_mut();
        (std::mem::take(&mut slot.0), std::mem::take(&mut slot.1))
    });
    let text = if name.is_empty() {
        String::new()
    } else {
        format!("{name}\n{message}")
    };
    unsafe { reply(out_ptr, out_len, text.into_bytes()) };
}

/// The number of ops registered with `inventory::submit!` across the linked
/// module. Zero means the life-before-main constructors never ran, and
/// every style is about to fail with "unknown op".
#[unsafe(no_mangle)]
pub extern "C" fn ezu_op_count() -> u32 {
    op_count() as u32
}

/// Every registered op name, newline-separated. Diagnostic: it says
/// *which* ops a module has, not just how many.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_op_names(out_ptr: u32, out_len: u32) {
    let names = ezu_paint::nodes::default_registry().op_names().join("\n");
    unsafe { reply(out_ptr, out_len, names.into_bytes()) };
}

/// How many ops `NodeRegistry::from_inventory()` finds. Also the smoke test
/// for life-before-main — see [`ezu_op_count`].
pub fn op_count() -> usize {
    ezu_paint::nodes::default_registry().op_names().len()
}

/// Whether the module was compiled with `+simd128`. The shipped module is;
/// it is both faster and smaller, and a host that built its own may want to
/// say which it has.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_simd_enabled() -> u32 {
    cfg!(target_feature = "simd128") as u32
}

/// Wasm linear memory currently committed to this module, in bytes.
///
/// This is the figure a host's memory cap applies to, and the one to watch
/// to shed load before an allocation fails. It never falls: freed Rust
/// values return to the allocator for reuse, but wasm cannot hand pages
/// back, so a drop in demand leaves the number at its peak and only a fresh
/// instance resets it.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_heap_bytes() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        // `memory_size` counts 64 KiB pages of memory 0.
        (core::arch::wasm32::memory_size(0) as u64) * 65536
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0
    }
}

// --- logging ---------------------------------------------------------------

/// Install the log sink at `level` (0 off, 1 error, 2 warn, 3 info,
/// 4 debug, 5 trace). Idempotent; the level of the first call wins.
///
/// Events are buffered for [`ezu_drain_logs`] rather than written to
/// stdout: a wasip1 module's stdio is torn down with the instance, and a
/// host that renders and then closes can lose whatever was still in the
/// pipe. Pulling them is also what lets a host attribute lines to the tile
/// it was rendering.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_log_init(level: u32) -> i64 {
    log::init(level)
}

/// Take every buffered log line, newline-separated, and empty the buffer.
/// Each line is `"<level> <target>: <message> k=v …"`. No timestamp: the
/// host knows when it drained, and a clock here would be one more WASI
/// import for nothing.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_drain_logs(out_ptr: u32, out_len: u32) {
    unsafe { reply(out_ptr, out_len, log::drain().into_bytes()) };
}

// --- renderer lifecycle ----------------------------------------------------

/// Parse a style document and build its graph. Returns a positive handle,
/// or a negative code.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_renderer_new(style_ptr: u32, style_len: u32) -> i64 {
    let text = match unsafe { input_str(style_ptr, style_len, "style document") } {
        Ok(t) => t,
        Err(code) => return code,
    };
    let renderer = match Renderer::new(text) {
        Ok(r) => r,
        Err(e) => return fail_err(e),
    };
    let handle = NEXT_HANDLE.with(|n| {
        let mut n = n.borrow_mut();
        let h = *n;
        *n += 1;
        h
    });
    RENDERERS.with(|r| r.borrow_mut().insert(handle, renderer));
    handle as i64
}

/// Drop a renderer and everything it holds.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_renderer_free(handle: u32) -> i64 {
    RENDERERS.with(|r| {
        if r.borrow_mut().remove(&handle).is_some() {
            0
        } else {
            bad_handle(handle)
        }
    })
}

/// Replace the active style. Returns the new node count. Invalidates the
/// intermediate cache and drops any pending source bindings.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_set_style(handle: u32, style_ptr: u32, style_len: u32) -> i64 {
    let text = match unsafe { input_str(style_ptr, style_len, "style document") } {
        Ok(t) => t,
        Err(code) => return code,
    };
    with_renderer_mut(handle, |r| match r.set_style(text) {
        Ok(n) => n as i64,
        Err(e) => fail_err(e),
    })
}

/// `tile-size` declared by the current style.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_tile_size(handle: u32) -> i64 {
    with_renderer(handle, |r| r.tile_size() as i64)
}

// --- queries ---------------------------------------------------------------

/// JSON Schema for the current style's `params` — types, defaults, ranges,
/// descriptions. The same document the CLI's tile server serves at
/// `/style/params`. Generate sliders and colour pickers from this rather
/// than parsing the style; it follows `ezu_set_style`, so a params panel
/// driven off it cannot drift from the graph being rendered.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_params_schema(handle: u32, out_ptr: u32, out_len: u32) -> i64 {
    with_renderer(handle, |r| unsafe {
        reply_json(out_ptr, out_len, r.params_schema().to_string())
    })
}

/// Effective attribution declared by the style (document + sources), joined
/// with ` | `, as plain text. Returns 1 when the style declares any, 0 when
/// it declares none (and writes an empty buffer). Upstream TileJSON /
/// PMTiles metadata is the host's concern — merge it on that side.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_attribution(handle: u32, out_ptr: u32, out_len: u32) -> i64 {
    with_renderer(handle, |r| match r.attribution() {
        Some(text) => {
            unsafe { reply(out_ptr, out_len, text.into_bytes()) };
            1
        }
        None => {
            unsafe { reply(out_ptr, out_len, Vec::new()) };
            0
        }
    })
}

/// The style's declared `legend` as JSON text. Returns 1 when there is one,
/// 0 when the style declares none. `zoom` past 30 means "every entry";
/// otherwise only the entries that apply at that zoom are kept.
///
/// Entries name the node that draws the symbol rather than restating a
/// colour, so a host lays out the labels and asks the map itself for the
/// swatches.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_legend(handle: u32, zoom: u32, out_ptr: u32, out_len: u32) -> i64 {
    let zoom = if zoom > 30 { None } else { Some(zoom as u8) };
    with_renderer(handle, |r| match r.legend_json(zoom) {
        Ok(Some(json)) => {
            unsafe { reply(out_ptr, out_len, json.into_bytes()) };
            1
        }
        Ok(None) => {
            unsafe { reply(out_ptr, out_len, Vec::new()) };
            0
        }
        Err(e) => fail_err(e),
    })
}

/// Names of every source with at least one pending binding, as a JSON
/// array. Order matches the style's `sources` declaration order.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_bound_sources(handle: u32, out_ptr: u32, out_len: u32) -> i64 {
    with_renderer(handle, |r| {
        let json = serde_json::to_string(&r.bound_sources()).unwrap_or_else(|_| "[]".into());
        unsafe { reply_json(out_ptr, out_len, json) }
    })
}

/// The tile a host should actually fetch from `name` in order to draw
/// `z/x/y`, as JSON `{"z":…,"x":…,"y":…}`.
///
/// For a source that declares `max-zoom`, a request past the ceiling
/// answers with the covering ancestor — bind those bytes with
/// `{"sourceZoom": <returned z>}` and the renderer resamples or reprojects
/// them into the requested tile. This exists so the ceiling lives in one
/// place; a host that hard-codes each source's maxzoom keeps a second copy
/// of something the style already states, and the two drift.
///
/// `x` and `y` may be off the tile grid, which is what a host walking a
/// neighbourhood hands in: `x` wraps around the antimeridian, `y` does not.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_source_tile(
    handle: u32,
    name_ptr: u32,
    name_len: u32,
    z: u32,
    x: i32,
    y: i32,
    out_ptr: u32,
    out_len: u32,
) -> i64 {
    let name = match unsafe { input_str(name_ptr, name_len, "source name") } {
        Ok(n) => n,
        Err(code) => return code,
    };
    with_renderer(handle, |r| match r.source_tile(name, z as u8, x, y) {
        Ok((tz, tx, ty)) => {
            let json = serde_json::json!({ "z": tz, "x": tx, "y": ty }).to_string();
            unsafe { reply_json(out_ptr, out_len, json) }
        }
        Err(e) => fail_err(e),
    })
}

/// Neighbour tile offsets the active style actually asks for from `name`,
/// as JSON `[[dx, dy], …]` (never including the centre `[0, 0]`).
///
/// Bind each offset listed here and the render gets exactly what the recipe
/// needs — neither a blind 3×3 per source nor, more costly, a window short
/// of what a stitched source wanted. An empty array means the centre tile
/// is enough.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_requested_neighbor_offsets(
    handle: u32,
    name_ptr: u32,
    name_len: u32,
    out_ptr: u32,
    out_len: u32,
) -> i64 {
    let name = match unsafe { input_str(name_ptr, name_len, "source name") } {
        Ok(n) => n,
        Err(code) => return code,
    };
    with_renderer(handle, |r| match r.requested_neighbor_offsets(name) {
        Ok(offsets) => {
            let pairs: Vec<[i32; 2]> = offsets.into_iter().map(|(dx, dy)| [dx, dy]).collect();
            let json = serde_json::to_string(&pairs).unwrap_or_else(|_| "[]".into());
            unsafe { reply_json(out_ptr, out_len, json) }
        }
        Err(e) => fail_err(e),
    })
}

/// Codepoints the currently bound features can require, as JSON
/// `{"<glyphs source>": [<codepoint>, …]}` sorted ascending.
///
/// This is the precise form of [`ezu_needed_glyph_ranges`]: a host that can
/// build its own glyph PBF transfers only the glyphs the tile draws instead
/// of the whole 256-codepoint block around each of them. On CJK labels that
/// is the difference between a few thousand glyphs and a few tens of
/// megabytes.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_needed_codepoints(handle: u32, out_ptr: u32, out_len: u32) -> i64 {
    with_renderer(handle, |r| match r.needed_codepoints() {
        Ok(map) => unsafe { reply_json(out_ptr, out_len, glyph_units_json(map)) },
        Err(e) => fail_err(e),
    })
}

/// Glyph ranges the currently bound features can require, as JSON
/// `{"<glyphs source>": [<range start>, …]}` where each number is a range
/// start (`0`, `256`, `512`, …) — i.e. the `{range}` in a
/// `…/{fontstack}/{range}.pbf` URL is `<start>-<start + 255>`.
///
/// For hosts that can only fetch whole `{range}.pbf` files. It is an
/// over-approximation, deliberately: it never omits a range a label needs,
/// and may name a few that go unused.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_needed_glyph_ranges(handle: u32, out_ptr: u32, out_len: u32) -> i64 {
    with_renderer(handle, |r| match r.needed_glyph_ranges() {
        Ok(map) => unsafe { reply_json(out_ptr, out_len, glyph_units_json(map)) },
        Err(e) => fail_err(e),
    })
}

fn glyph_units_json(map: std::collections::BTreeMap<&str, Vec<u32>>) -> String {
    serde_json::to_string(&map).unwrap_or_else(|_| "{}".into())
}

/// What this renderer is holding, in bytes, as JSON — so a host can shed
/// load *before* an allocation fails rather than after.
///
/// The keys are `ezu-wasm`'s: `heapBytes`, `glyphBytes`, `glyphRanges`,
/// `glyphBudget`, `fontBytes`, `imageBytes`, `cacheBytes`, `cacheBudget`.
/// `glyphBudget` is `null` when none is set — JSON has no `Infinity`, which
/// is what the JS shell reports there.
///
/// These are payload sizes, not an accounting of the heap: they omit
/// allocator overhead, decoded features, per-font glyph-path caches, and
/// the buffers a render is using right now. Expect the parts to sum to less
/// than `heapBytes`.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_memory_usage(handle: u32, out_ptr: u32, out_len: u32) -> i64 {
    with_renderer(handle, |r| {
        let u = r.memory_usage();
        let json = serde_json::json!({
            "heapBytes": ezu_heap_bytes(),
            "glyphBytes": u.glyph_bytes,
            "glyphRanges": u.glyph_ranges,
            "glyphBudget": u.glyph_budget,
            "fontBytes": u.font_bytes,
            "imageBytes": u.image_bytes,
            "cacheBytes": u.cache_bytes,
            "cacheBudget": u.cache_budget,
        })
        .to_string();
        unsafe { reply_json(out_ptr, out_len, json) }
    })
}

// --- binding ---------------------------------------------------------------

/// Bind raw bytes under a `sources.<name>` entry from the style. The
/// renderer dispatches on the source's declared `type`:
///
/// - `brush` → parse `.myb` JSON into the persistent brush bank
/// - `image` → decode PNG/WebP into the persistent image bank
/// - `sprite` → decode the atlas image plus its index (inline in the style,
///   or the `index` option) into the persistent sprite bank
/// - `font` → parse TTF/OTF/TTC bytes into the persistent font bank
/// - `glyphs` → decode one SDF glyph PBF into the persistent glyph bank;
///   glyphs are filed by id, so repeated calls accumulate and a payload may
///   be a whole `{range}.pbf` or any subset
/// - `mvt` / `pmtiles` → store raw MVT bytes per neighbour offset, decoded
///   and bound as `<source>.<layer>` at render time
/// - `dem` / `raster` → store raw bytes per neighbour offset, decoded and
///   3×3 stitched at render time
/// - `geojson` → *remote* GeoJSON only, projected per tile at render time;
///   inline `data` needs no binding
///
/// The first five are persistent and survive [`ezu_clear_sources`]; the
/// rest are tile-scoped and do not.
///
/// `opts` is JSON, and may be empty for the defaults:
/// `{"coord": [dx, dy], "sourceZoom": <z>, "index": "<sprite json text>"}`.
///
/// `coord` says which tile of the 3×3 neighbourhood these bytes are;
/// `[0, 0]`, the default, is the tile being rendered. `sourceZoom` declares
/// that the bytes are natively encoded at a *shallower* zoom than the tile
/// being rendered — a source that stops at its `max-zoom` while the host
/// serves deeper tiles — and the renderer reprojects or resamples them into
/// the tile's frame. Passing a source's `max-zoom` unconditionally is fine:
/// at or below the rendered zoom it does nothing.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn ezu_bind_source(
    handle: u32,
    name_ptr: u32,
    name_len: u32,
    bytes_ptr: u32,
    bytes_len: u32,
    opts_ptr: u32,
    opts_len: u32,
) -> i64 {
    let name = match unsafe { input_str(name_ptr, name_len, "source name") } {
        Ok(n) => n,
        Err(code) => return code,
    };
    let opts = match options::parse_bind(unsafe { input(opts_ptr, opts_len) }) {
        Ok(o) => o,
        Err(e) => return fail_err(e),
    };
    let bytes = unsafe { input(bytes_ptr, bytes_len) }.to_vec();
    with_renderer_mut(handle, |r| match r.bind_source(name, bytes, &opts) {
        Ok(()) => 0,
        Err(e) => fail_err(e),
    })
}

/// Drop every pending tile-scoped binding, keeping the persistent banks.
/// Call between tile renders.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_clear_sources(handle: u32) -> i64 {
    with_renderer_mut(handle, |r| {
        r.clear_sources();
        0
    })
}

/// Cap the glyph bytes each bound fontstack keeps resident.
///
/// Unset, a fontstack keeps every range ever bound to it for the life of
/// the renderer — `ezu_clear_sources` does not touch glyphs, and on a
/// long-lived instance rendering across a basemap that is usually what
/// grew. Trimming happens **after** each render, not while binding, so a
/// render never loses glyphs that were bound for it.
///
/// It is a per-fontstack ceiling: a style with a regular, a medium and an
/// italic stack can hold three times what is set here. This host cannot
/// refetch, so anything trimmed must be bound again before the next tile
/// that needs it — [`ezu_needed_codepoints`] names exactly what.
#[unsafe(no_mangle)]
pub extern "C" fn ezu_set_glyph_budget(handle: u32, bytes: u64) -> i64 {
    with_renderer_mut(handle, |r| {
        r.set_glyph_budget(bytes.min(usize::MAX as u64) as usize);
        0
    })
}

// --- rendering -------------------------------------------------------------

/// Render one tile using whatever sources are currently bound, and hand the
/// encoded bytes back in the out-slots.
///
/// `opts` is JSON, and may be empty for the defaults:
///
/// ```json
/// {
///   "format": "png",
///   "tileSize": 512,
///   "pad": 24,
///   "png": { "compression": "default" },
///   "params": { "name": 1.5 }
/// }
/// ```
///
/// `format` is `"png"` (default), `"webp"` (lossless) or `"rgba"` (straight
/// un-premultiplied 8-bit RGBA, no container). An unrecognised name is a
/// typo rather than a request for the default, and fails with
/// `InvalidStyle`. `tileSize` and `pad` override the style's canvas for
/// this call. `params` are render-time overrides for the style's declared
/// `params`, validated exactly the way the CLI's `--param` is; omitted
/// names keep their declared default.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn ezu_render_tile(
    handle: u32,
    z: u32,
    x: u32,
    y: u32,
    opts_ptr: u32,
    opts_len: u32,
    out_ptr: u32,
    out_len: u32,
) -> i64 {
    let opts = match options::parse_render(unsafe { input(opts_ptr, opts_len) }) {
        Ok(o) => o,
        Err(e) => return fail_err(e),
    };
    with_renderer(handle, |r| match r.render_tile(z as u8, x, y, opts) {
        Ok(bytes) => {
            unsafe { reply(out_ptr, out_len, bytes) };
            0
        }
        Err(e) => fail_err(e),
    })
}

#[cfg(test)]
mod tests;
