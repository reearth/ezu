// Package ezu embeds the ezu painterly map renderer as a wasm module and
// runs it on wazero: pure Go, no cgo, `go test` works out of the box.
//
// The shape is the one the renderer was built for: ezu fetches nothing. You
// bind the bytes — vector tiles, DEM or imagery tiles, GeoJSON, sprites,
// fonts, glyph ranges, brushes — and ask for a tile; the renderer owns the
// pixels and you own the I/O. It is the same bargain the JavaScript
// bindings strike, and the same [Renderer] surface, so a service and a
// browser can render from one style and agree byte for byte.
//
//	rt, err := ezu.NewRuntime(ctx)
//	defer rt.Close(ctx)
//
//	r, err := rt.NewRenderer(ctx, styleJSON)
//	defer r.Close(ctx)
//
//	r.BindSource(ctx, "basemap", mvtBytes, ezu.Bind{})
//	png, err := r.RenderTile(ctx, 14, 14554, 6454, ezu.Render{})
//	r.ClearSources(ctx)
//
// # Concurrency
//
// One [Renderer] is one wasm instance, and a wasm instance is
// single-threaded: it has one linear memory, one allocator, and no locks
// anywhere inside it. Two goroutines calling into the same instance would
// interleave two allocation sequences that were written assuming they could
// not, which is memory corruption rather than a slow answer. So the rule
// is: **one renderer, one goroutine at a time.**
//
// Nothing here serialises for you. A Renderer refuses a concurrent entry
// with [ErrConcurrentUse] rather than quietly queueing, because a caller
// who wanted parallel tiles and got a lock instead would get correct
// pixels, no error, and none of the throughput they asked for — and would
// find out in production. Rendering tiles in parallel means several
// renderers: build them yourself, or take them from a [Pool], which exists
// for exactly this.
//
// [Runtime] is the part that is shared. It holds the compiled module, which
// is the expensive artefact; instantiating from it is a fresh linear memory
// and little else. It is safe for concurrent use.
//
// # The module
//
// ezu_cabi.wasm, embedded, is built from crates/ezu-cabi, whose src/lib.rs
// documents the ABI this package implements. Regenerate it with
// `go generate ./...`.
package ezu

import (
	"context"
	_ "embed"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"sync"
	"sync/atomic"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/imports/wasi_snapshot_preview1"
)

//go:embed ezu_cabi.wasm
var wasmModule []byte

// The ABI this package was written against. ezu_abi_version must agree or
// the embedded module is stale — which is the cost of committing a built
// artefact, and the check that keeps the cost cheap.
const abiVersion = 1

// The host module the wasm side imports its out-of-memory report from. See
// oomHandler and crates/ezu-cabi/src/oom.rs.
const hostModule = "ezu_host"

// Runtime is the module compiled once, so that many instances of it are
// cheap. Safe for concurrent use.
//
// Compiling is by far the expensive part — several seconds for a module
// this size — and it is what [WithCompilationCache] lets a process skip on
// the next start.
type Runtime struct {
	runtime  wazero.Runtime
	compiled wazero.CompiledModule
	// Instance name → the renderer it belongs to, so a host call made from
	// inside the module can find whose allocator failed.
	mu        sync.Mutex
	renderers map[string]*Renderer
	nextName  uint64
}

// Option configures a [Runtime].
type Option func(*config)

type config struct {
	moduleBytes      []byte
	cache            wazero.CompilationCache
	memoryLimitPages uint32
}

// WithModule runs a module the caller supplies instead of the embedded one
// — a build with different flags, or one produced by CI to check that the
// committed artefact still matches the source.
func WithModule(wasm []byte) Option {
	return func(c *config) { c.moduleBytes = wasm }
}

// WithCompilationCache reuses wazero's compiled form across processes, so
// only the first start pays to compile the module.
//
//	cache, err := wazero.NewCompilationCacheWithDir("/var/cache/ezu")
//	rt, err := ezu.NewRuntime(ctx, ezu.WithCompilationCache(cache))
//
// The cache is keyed by the module's bytes and by wazero's own version, so
// a rebuilt module or an upgraded wazero recompiles rather than serving
// something stale. Close the cache when the last runtime using it is done.
func WithCompilationCache(cache wazero.CompilationCache) Option {
	return func(c *config) { c.cache = cache }
}

// WithMemoryLimit caps the linear memory any one renderer from this runtime
// may grow to, in bytes, rounded up to wasm's 64 KiB page. Zero, the
// default, means the wasm maximum (4 GiB) and leaves the limiting to
// whatever the process runs under.
//
// There is deliberately no default number here. What a render needs is a
// property of the style and the tile size — a 512 px tile through a few
// blurs is not the same as a 1024 px one through a brush engine and a CJK
// label pass — and a number picked in this package would be wrong for
// somebody. Measure your styles with [Renderer.MemoryUsage] and set it from
// what you see.
//
// Hitting the limit is not a silent kill: the module's allocator reports
// the failed allocation on its way out, and the call returns an [Error]
// named OutOfMemory carrying the number of bytes it could not have. That
// instance is finished afterwards — see [ErrRendererDead].
//
// The cap is per renderer but set on the runtime, so every renderer from
// one runtime gets the same one. Two caps means two runtimes; a runtime is
// only expensive because of the compile, and [WithCompilationCache] makes
// the second one cheap.
func WithMemoryLimit(bytes uint64) Option {
	return func(c *config) {
		pages := (bytes + 65535) / 65536
		if pages > 65536 {
			pages = 65536
		}
		c.memoryLimitPages = uint32(pages)
	}
}

// NewRuntime compiles the module. Close it when the last renderer is done.
func NewRuntime(ctx context.Context, opts ...Option) (*Runtime, error) {
	cfg := config{moduleBytes: wasmModule}
	for _, opt := range opts {
		opt(&cfg)
	}

	runtimeConfig := wazero.NewRuntimeConfig()
	if cfg.cache != nil {
		runtimeConfig = runtimeConfig.WithCompilationCache(cfg.cache)
	}
	if cfg.memoryLimitPages > 0 {
		runtimeConfig = runtimeConfig.WithMemoryLimitPages(cfg.memoryLimitPages)
	}

	runtime := wazero.NewRuntimeWithConfig(ctx, runtimeConfig)
	wasi_snapshot_preview1.MustInstantiate(ctx, runtime)

	rt := &Runtime{runtime: runtime, renderers: map[string]*Renderer{}}
	if err := rt.instantiateHost(ctx); err != nil {
		runtime.Close(ctx)
		return nil, err
	}

	compiled, err := runtime.CompileModule(ctx, cfg.moduleBytes)
	if err != nil {
		runtime.Close(ctx)
		return nil, fmt.Errorf("ezu: compiling the module: %w", err)
	}
	rt.compiled = compiled
	return rt, nil
}

// Close releases the runtime and every renderer instantiated from it.
func (r *Runtime) Close(ctx context.Context) error { return r.runtime.Close(ctx) }

// instantiateHost exports the one function the module imports:
// ezu_host.oom, which its global allocator calls when the heap cannot grow.
//
// It must not return normally. Rust's allocator has no answer to give its
// caller at that point — the wasm side does `unreachable` if this ever
// comes back — so the host panics, which wazero converts into a trap and
// hands back as an error from the exported call that was running. That is
// the outcome we want either way: an instance whose allocation failed
// mid-render has half-built values and an allocator whose bookkeeping is
// whatever it was, and is finished. What the panic buys is the *number* —
// the render fails with "asked for N bytes" instead of a bare unreachable.
func (r *Runtime) instantiateHost(ctx context.Context) error {
	_, err := r.runtime.NewHostModuleBuilder(hostModule).
		NewFunctionBuilder().
		WithFunc(func(ctx context.Context, mod api.Module, requested uint32) {
			r.mu.Lock()
			renderer := r.renderers[mod.Name()]
			r.mu.Unlock()
			if renderer != nil {
				renderer.oomBytes.Store(uint64(requested))
			}
			panic(oomPanic{requested: requested})
		}).
		Export("oom").
		Instantiate(ctx)
	if err != nil {
		return fmt.Errorf("ezu: exporting the %s module: %w", hostModule, err)
	}
	return nil
}

// oomPanic is what the host's oom function panics with. wazero recovers it
// and reports it as a trap; the value is not read back out of the error, it
// is only there so a stray panic in this package is distinguishable from
// this one in a stack trace.
type oomPanic struct{ requested uint32 }

// Renderer is one style, one wasm instance, and whatever sources are bound
// to it.
//
// It is **not** safe for concurrent use, and says so rather than hiding it:
// a second goroutine entering a call already in progress gets
// [ErrConcurrentUse] and no side effects. See the package documentation for
// why that is an error rather than a lock, and [Pool] for rendering tiles
// in parallel.
type Renderer struct {
	rt     *Runtime
	module api.Module
	name   string
	handle uint32

	// Non-blocking guard. Not a mutex: the rule is one goroutine at a
	// time, and a mutex would turn breaking it into a performance mystery
	// instead of an error.
	busy atomic.Bool
	// Set once the instance has trapped, after which nothing about it is
	// trustworthy.
	dead atomic.Bool
	// Bytes the allocator last failed to get, recorded by the host call
	// before it trapped.
	oomBytes atomic.Uint64
}

// NewRenderer instantiates the module and builds a renderer from a style
// document. Close it when done; it holds a wasm instance and its whole
// linear memory.
func (r *Runtime) NewRenderer(ctx context.Context, styleJSON []byte) (*Renderer, error) {
	r.mu.Lock()
	r.nextName++
	name := fmt.Sprintf("ezu-%d", r.nextName)
	r.mu.Unlock()

	renderer, err := r.instantiate(ctx, name, startBothNets)
	if err != nil {
		return nil, err
	}
	handle, err := renderer.call(ctx, func(ctx context.Context) (int64, error) {
		ptr, err := renderer.writeBytes(ctx, styleJSON)
		if err != nil {
			return 0, err
		}
		defer renderer.free(ctx, ptr, uint32(len(styleJSON)))
		return renderer.call1(ctx, "ezu_renderer_new", uint64(ptr), uint64(len(styleJSON)))
	})
	if err != nil {
		renderer.Close(ctx)
		return nil, err
	}
	renderer.handle = uint32(handle)
	return renderer, nil
}

// startMode says which of the two ways into the module's life-before-main
// constructors an instance gets. Only [startBothNets] is used outside
// tests; the others exist so the tests can show that the nets are
// load-bearing rather than decorative.
type startMode int

const (
	// Both: ask wazero to run _initialize, then call __wasm_call_ctors too.
	startBothNets startMode = iota
	// The second net alone, which is the spelling a host that does not run
	// start functions ends up with.
	startCtorsOnly
	// Neither, which leaves the node registry empty.
	startNoNets
)

// instantiate builds one instance and runs the constructors.
func (r *Runtime) instantiate(ctx context.Context, name string, start startMode) (*Renderer, error) {
	config := wazero.NewModuleConfig().WithName(name)
	if start == startBothNets {
		config = config.WithStartFunctions("_initialize")
	} else {
		config = config.WithStartFunctions()
	}

	renderer := &Renderer{rt: r, name: name}
	// Registered before instantiation: `_initialize` runs during it, and
	// anything it allocates could in principle be what fails.
	r.mu.Lock()
	r.renderers[name] = renderer
	r.mu.Unlock()

	module, err := r.runtime.InstantiateModule(ctx, r.compiled, config)
	if err != nil {
		r.forget(name)
		return nil, fmt.Errorf("ezu: instantiating the module: %w", err)
	}
	renderer.module = module

	// Belt and braces for the life-before-main problem. ezu registers every
	// node op with `inventory::submit!`, from a constructor that runs
	// before main — and in a wasm module "before main" only happens if
	// somebody asks for it. The module is pinned to the reactor shape by
	// crates/ezu-cabi/build.rs, so the constructors run from `_initialize`
	// or from `__wasm_call_ctors` and from nowhere else; a module that got
	// neither has an empty node registry and fails every style with
	// "unknown op", saying nothing about why.
	//
	// Calling __wasm_call_ctors as well costs one call and is safe to
	// repeat: on wasm, inventory guards each submission with an AtomicBool,
	// so a second run registers nothing twice.
	if start != startNoNets && renderer.HasExport("__wasm_call_ctors") {
		if _, err := renderer.call1(ctx, "__wasm_call_ctors"); err != nil {
			renderer.Close(ctx)
			return nil, err
		}
	}

	got, err := renderer.call1(ctx, "ezu_abi_version")
	if err != nil {
		renderer.Close(ctx)
		return nil, err
	}
	if uint32(got) != abiVersion {
		renderer.Close(ctx)
		return nil, fmt.Errorf(
			"ezu: the module speaks ABI %d, this package speaks %d — rebuild it (go generate ./...)",
			got, abiVersion)
	}
	return renderer, nil
}

func (r *Runtime) forget(name string) {
	r.mu.Lock()
	delete(r.renderers, name)
	r.mu.Unlock()
}

// Close releases the renderer's wasm instance and all of its memory.
func (r *Renderer) Close(ctx context.Context) error {
	r.rt.forget(r.name)
	if r.module == nil {
		return nil
	}
	return r.module.Close(ctx)
}

// HasExport reports whether the module exports a function by that name.
func (r *Renderer) HasExport(name string) bool {
	return r.module.ExportedFunction(name) != nil
}

// --- errors ---------------------------------------------------------------

// ErrConcurrentUse is returned when two goroutines enter one [Renderer] at
// once. It is a bug in the caller, not a transient condition: retrying will
// sometimes work and sometimes interleave, which is the worst of both. Use
// one renderer per goroutine, or a [Pool].
var ErrConcurrentUse = errors.New("ezu: a renderer is being used from two goroutines at once; use one renderer per goroutine (see ezu.Pool)")

// ErrRendererDead is returned by every call on a renderer whose instance
// has trapped — after an [Error] named OutOfMemory, most likely. Nothing
// about that instance's memory is trustworthy afterwards; close it and
// build another.
var ErrRendererDead = errors.New("ezu: this renderer's wasm instance has trapped and cannot be used again")

// Error is a failure reported by the renderer itself, carrying the kind's
// name so a caller can branch on it.
//
// The names are a contract shared with ezu's JavaScript bindings, where
// they arrive as a thrown Error's .name — the same failure has the same
// name in both, so a service and a browser can share the handling of it:
//
//	InvalidStyle   BrushParse    MvtDecode     DemDecode
//	RasterDecode   GeoJsonDecode SpriteDecode  FontParse
//	GlyphDecode    RenderFailed  PngEncode     WebpEncode
//	UnknownSource  OutOfMemory
//
// One name has no JavaScript counterpart: BadHandle, which this package
// should never let a caller see.
type Error struct {
	// Name is the failure kind, from the list above.
	Name string
	// Message is the human-readable detail, without the name.
	Message string
	// Code is the module's numeric return. A cheaper copy of Name, and not
	// the contract: a kind added later may be one this package's version
	// does not know a number for.
	Code int64
	// Bytes is what the allocator could not have, for an OutOfMemory.
	Bytes uint64
}

func (e *Error) Error() string {
	if e.Name == KindOutOfMemory && e.Bytes > 0 {
		return fmt.Sprintf("ezu: %s: %s (%d bytes)", e.Name, e.Message, e.Bytes)
	}
	return fmt.Sprintf("ezu: %s: %s", e.Name, e.Message)
}

// The error names worth naming as constants: the ones a caller is most
// likely to branch on. The rest are strings from the list on [Error], and
// comparing to a literal is fine.
const (
	KindOutOfMemory  = "OutOfMemory"
	KindInvalidStyle = "InvalidStyle"
	KindUnknownSrc   = "UnknownSource"
	KindRenderFailed = "RenderFailed"
)

// IsKind reports whether err is an [Error] with that name.
//
//	if ezu.IsKind(err, ezu.KindOutOfMemory) { … }
func IsKind(err error, name string) bool {
	var e *Error
	return errors.As(err, &e) && e.Name == name
}

// --- calling --------------------------------------------------------------

// call runs one ABI interaction under the concurrency guard, and converts a
// trap into a typed error.
func (r *Renderer) call(ctx context.Context, f func(context.Context) (int64, error)) (int64, error) {
	if r.dead.Load() {
		return 0, ErrRendererDead
	}
	if !r.busy.CompareAndSwap(false, true) {
		return 0, ErrConcurrentUse
	}
	defer r.busy.Store(false)

	got, err := f(ctx)
	if err != nil {
		// A trap ends the instance whatever caused it.
		r.dead.Store(true)
		if bytes := r.oomBytes.Load(); bytes > 0 {
			return 0, &Error{
				Name:    KindOutOfMemory,
				Message: "the wasm heap could not grow; this renderer is finished",
				Code:    -14,
				Bytes:   bytes,
			}
		}
		return 0, err
	}
	if got < 0 {
		return 0, r.errorOf(ctx, got)
	}
	return got, nil
}

func (r *Renderer) call1(ctx context.Context, name string, args ...uint64) (int64, error) {
	fn := r.module.ExportedFunction(name)
	if fn == nil {
		return 0, fmt.Errorf("ezu: the module exports no %s", name)
	}
	out, err := fn.Call(ctx, args...)
	if err != nil {
		return 0, fmt.Errorf("ezu: calling %s: %w", name, err)
	}
	if len(out) == 0 {
		return 0, nil
	}
	return int64(out[0]), nil
}

// errorOf turns a negative return into a typed error by reading the
// module's last-error slot, which holds "<Name>\n<message>".
func (r *Renderer) errorOf(ctx context.Context, code int64) error {
	unknown := func(reason error) error {
		return &Error{
			Name:    "Unknown",
			Message: fmt.Sprintf("error %d, and the detail could not be read: %v", code, reason),
			Code:    code,
		}
	}
	slots, err := r.alloc(ctx, 8)
	if err != nil {
		return unknown(err)
	}
	defer r.free(ctx, slots, 8)
	if _, err := r.call1(ctx, "ezu_last_error", uint64(slots), uint64(slots+4)); err != nil {
		return unknown(err)
	}
	raw, err := r.takeReply(ctx, slots)
	if err != nil {
		return unknown(err)
	}
	name, message, found := strings.Cut(string(raw), "\n")
	if !found && name == "" {
		return unknown(errors.New("the module reported no detail"))
	}
	return &Error{Name: name, Message: message, Code: code}
}

// --- linear memory --------------------------------------------------------

func (r *Renderer) alloc(ctx context.Context, n uint32) (uint32, error) {
	ptr, err := r.call1(ctx, "ezu_alloc", uint64(n))
	if err != nil {
		return 0, err
	}
	if ptr == 0 && n > 0 {
		return 0, fmt.Errorf("ezu: the module could not allocate %d bytes", n)
	}
	return uint32(ptr), nil
}

func (r *Renderer) free(ctx context.Context, ptr, n uint32) {
	_, _ = r.call1(ctx, "ezu_free", uint64(ptr), uint64(n))
}

func (r *Renderer) writeBytes(ctx context.Context, b []byte) (uint32, error) {
	ptr, err := r.alloc(ctx, uint32(len(b)))
	if err != nil {
		return 0, err
	}
	if len(b) > 0 && !r.module.Memory().Write(ptr, b) {
		return 0, fmt.Errorf("ezu: %d bytes do not fit in the module's memory", len(b))
	}
	return ptr, nil
}

// takeReply reads the (ptr, len) the module wrote into the out-slots, copies
// the buffer out, and frees it: whoever receives a buffer owns it.
func (r *Renderer) takeReply(ctx context.Context, slots uint32) ([]byte, error) {
	raw, ok := r.module.Memory().Read(slots, 8)
	if !ok {
		return nil, errors.New("ezu: the out-slots are outside the module's memory")
	}
	ptr := binary.LittleEndian.Uint32(raw[0:4])
	n := binary.LittleEndian.Uint32(raw[4:8])
	if n == 0 {
		return nil, nil
	}
	body, ok := r.module.Memory().Read(ptr, n)
	if !ok {
		return nil, fmt.Errorf("ezu: a reply of %d bytes at %d is outside the module's memory", n, ptr)
	}
	out := make([]byte, n)
	copy(out, body)
	r.free(ctx, ptr, n)
	return out, nil
}

// withSlots runs f with a pair of out-slots allocated, and returns whatever
// the module wrote into them along with f's return code.
func (r *Renderer) withSlots(ctx context.Context, f func(slots uint32) (int64, error)) (int64, []byte, error) {
	var payload []byte
	code, err := r.call(ctx, func(ctx context.Context) (int64, error) {
		slots, err := r.alloc(ctx, 8)
		if err != nil {
			return 0, err
		}
		defer r.free(ctx, slots, 8)
		code, err := f(slots)
		if err != nil || code < 0 {
			return code, err
		}
		payload, err = r.takeReply(ctx, slots)
		return code, err
	})
	return code, payload, err
}

// --- options --------------------------------------------------------------

// Bind is the per-binding options object. The zero value binds the tile
// being rendered, at its own zoom, with no sprite index.
type Bind struct {
	// DX, DY place these bytes within the 3×3 neighbourhood. (0, 0), the
	// zero value, is the tile being rendered.
	//
	// Cross-tile label collision and edge-continuous DEM or raster shading
	// are the only things that read neighbours, and only for the sources
	// that ask: [Renderer.RequestedNeighborOffsets] says which, so a host
	// binds exactly the window the style needs rather than a blind 3×3.
	DX, DY int
	// SourceZoom declares that the bytes are natively encoded at a
	// shallower zoom than the tile being rendered — a source that stops at
	// its max-zoom while you serve deeper tiles — and the renderer
	// reprojects or resamples them into the tile's frame. Zero means "not
	// set"; ask [Renderer.SourceTile] which tile to fetch and pass the zoom
	// it answers with, which is a no-op below the ceiling.
	SourceZoom int
	// Index is a sprite source's index document, when the style gives a URL
	// for it rather than inlining it. Ignored by every other source kind.
	Index string
}

func (b Bind) json() ([]byte, error) {
	if b == (Bind{}) {
		return nil, nil
	}
	payload := map[string]any{"coord": [2]int{b.DX, b.DY}}
	if b.SourceZoom != 0 {
		payload["sourceZoom"] = b.SourceZoom
	}
	if b.Index != "" {
		payload["index"] = b.Index
	}
	return json.Marshal(payload)
}

// Format is the encoding [Renderer.RenderTile] hands back.
type Format string

const (
	// PNG, compressed per [Render.PNGCompression]. The default.
	FormatPNG Format = "png"
	// Lossless WebP.
	FormatWebP Format = "webp"
	// Straight un-premultiplied 8-bit RGBA, no container: tile-size squared
	// times four bytes, row-major.
	FormatRGBA Format = "rgba"
)

// Compression is how much effort the PNG encoder spends. Ignored by the
// other formats.
type Compression string

const (
	CompressionFast    Compression = "fast"
	CompressionDefault Compression = "default"
	CompressionBest    Compression = "best"
)

// Render is the per-render options object. The zero value renders PNG at
// the style's own canvas size with the style's own parameter defaults.
type Render struct {
	// Format of the returned bytes. Empty means [FormatPNG].
	Format Format
	// TileSize and Pad override the style's canvas for this render — a
	// hi-DPI tile, or a cheap preview. Zero keeps the style's own, with pad
	// floored by whatever the graph's filters reach for.
	TileSize, Pad uint32
	// PNGCompression is the encoder effort. Empty means
	// [CompressionDefault].
	PNGCompression Compression
	// Params are render-time overrides for the style's declared params,
	// validated the same way the CLI's --param is. Values may be numbers,
	// booleans or strings; omitted names keep their declared default.
	// [Renderer.ParamsSchema] describes what a style accepts.
	Params map[string]any
}

func (o Render) json() ([]byte, error) {
	payload := map[string]any{}
	if o.Format != "" {
		payload["format"] = string(o.Format)
	}
	if o.TileSize != 0 {
		payload["tileSize"] = o.TileSize
	}
	if o.Pad != 0 {
		payload["pad"] = o.Pad
	}
	if o.PNGCompression != "" {
		payload["png"] = map[string]any{"compression": string(o.PNGCompression)}
	}
	if len(o.Params) > 0 {
		payload["params"] = o.Params
	}
	if len(payload) == 0 {
		return nil, nil
	}
	return json.Marshal(payload)
}

// --- the surface ----------------------------------------------------------

// SetStyle replaces the active style and returns the new node count. It
// invalidates the intermediate cache and drops every pending source
// binding, but keeps the persistent banks — brushes, fonts, sprites,
// glyphs, images — so a style swap on a live renderer does not re-fetch
// them.
func (r *Renderer) SetStyle(ctx context.Context, styleJSON []byte) (int, error) {
	n, err := r.call(ctx, func(ctx context.Context) (int64, error) {
		ptr, err := r.writeBytes(ctx, styleJSON)
		if err != nil {
			return 0, err
		}
		defer r.free(ctx, ptr, uint32(len(styleJSON)))
		return r.call1(ctx, "ezu_set_style",
			uint64(r.handle), uint64(ptr), uint64(len(styleJSON)))
	})
	return int(n), err
}

// TileSize is the tile-size declared by the current style.
func (r *Renderer) TileSize(ctx context.Context) (uint32, error) {
	n, err := r.call(ctx, func(ctx context.Context) (int64, error) {
		return r.call1(ctx, "ezu_tile_size", uint64(r.handle))
	})
	return uint32(n), err
}

// BindSource binds raw bytes under a sources.<name> entry from the style.
// The renderer dispatches on the source's declared type:
//
//   - brush — a .myb JSON document, into the persistent brush bank
//   - image — PNG or WebP, into the persistent image bank
//   - sprite — the atlas image, with its index inline in the style or in
//     [Bind.Index]
//   - font — TTF, OTF or TTC bytes
//   - glyphs — one SDF glyph PBF. Glyphs are filed by id, so repeated calls
//     accumulate and a payload may be a whole {range}.pbf or any subset
//   - mvt, pmtiles — vector tile bytes, per [Bind.DX]/[Bind.DY]
//   - dem, raster — tile bytes, decoded and 3×3 stitched at render time
//   - geojson — a remote GeoJSON document; inline data needs no binding
//
// The first five are persistent and survive [Renderer.ClearSources]; the
// rest are tile-scoped and do not.
//
// This host cannot fetch anything on its own, so everything a render needs
// must be bound before it: [Renderer.RequestedNeighborOffsets] names the
// neighbour tiles, and [Renderer.NeededCodepoints] the glyphs. Text whose
// glyphs are missing is dropped with a warning, which reaches you through
// [Renderer.DrainLogs] and not as an error.
func (r *Renderer) BindSource(ctx context.Context, name string, data []byte, opts Bind) error {
	optsJSON, err := opts.json()
	if err != nil {
		return fmt.Errorf("ezu: encoding the bind options: %w", err)
	}
	_, err = r.call(ctx, func(ctx context.Context) (int64, error) {
		namePtr, err := r.writeBytes(ctx, []byte(name))
		if err != nil {
			return 0, err
		}
		defer r.free(ctx, namePtr, uint32(len(name)))
		dataPtr, err := r.writeBytes(ctx, data)
		if err != nil {
			return 0, err
		}
		defer r.free(ctx, dataPtr, uint32(len(data)))
		optsPtr, err := r.writeBytes(ctx, optsJSON)
		if err != nil {
			return 0, err
		}
		defer r.free(ctx, optsPtr, uint32(len(optsJSON)))
		return r.call1(ctx, "ezu_bind_source",
			uint64(r.handle),
			uint64(namePtr), uint64(len(name)),
			uint64(dataPtr), uint64(len(data)),
			uint64(optsPtr), uint64(len(optsJSON)))
	})
	return err
}

// ClearSources drops every pending tile-scoped binding, keeping the
// persistent banks. Call it between tiles.
func (r *Renderer) ClearSources(ctx context.Context) error {
	_, err := r.call(ctx, func(ctx context.Context) (int64, error) {
		return r.call1(ctx, "ezu_clear_sources", uint64(r.handle))
	})
	return err
}

// BoundSources names every source with at least one pending binding, in the
// style's declaration order.
func (r *Renderer) BoundSources(ctx context.Context) ([]string, error) {
	var out []string
	_, payload, err := r.withSlots(ctx, func(slots uint32) (int64, error) {
		return r.call1(ctx, "ezu_bound_sources", uint64(r.handle), uint64(slots), uint64(slots+4))
	})
	if err != nil {
		return nil, err
	}
	if err := json.Unmarshal(payload, &out); err != nil {
		return nil, fmt.Errorf("ezu: reading the bound sources: %w", err)
	}
	return out, nil
}

// SetGlyphBudget caps the glyph bytes each bound fontstack keeps resident.
//
// Unset, a fontstack keeps every range ever bound to it for the life of the
// renderer — [Renderer.ClearSources] does not touch glyphs, and on a
// long-lived instance rendering across a basemap that is usually what grew.
// Trimming happens after each render, never while binding, so a render
// cannot lose glyphs that were bound for it.
//
// It is a per-fontstack ceiling: a style with a regular, a medium and an
// italic stack can hold three times this. Anything trimmed must be bound
// again before the next tile that needs it; a host that re-binds every tile
// rather than tracking what it sent needs no other change.
func (r *Renderer) SetGlyphBudget(ctx context.Context, bytes uint64) error {
	_, err := r.call(ctx, func(ctx context.Context) (int64, error) {
		return r.call1(ctx, "ezu_set_glyph_budget", uint64(r.handle), bytes)
	})
	return err
}

// Attribution is the effective attribution declared by the style (document
// and sources), joined with " | ". Empty when the style declares none.
// Upstream TileJSON or PMTiles metadata is yours to merge in.
func (r *Renderer) Attribution(ctx context.Context) (string, error) {
	_, payload, err := r.withSlots(ctx, func(slots uint32) (int64, error) {
		return r.call1(ctx, "ezu_attribution", uint64(r.handle), uint64(slots), uint64(slots+4))
	})
	return string(payload), err
}

// ParamsSchema is the JSON Schema for the current style's params — types,
// defaults, ranges, descriptions. The same document the CLI's tile server
// serves at /style/params.
//
// Generate your controls from this rather than parsing the style: it
// follows [Renderer.SetStyle], so a panel driven off it cannot drift from
// the graph being rendered.
func (r *Renderer) ParamsSchema(ctx context.Context) (json.RawMessage, error) {
	_, payload, err := r.withSlots(ctx, func(slots uint32) (int64, error) {
		return r.call1(ctx, "ezu_params_schema", uint64(r.handle), uint64(slots), uint64(slots+4))
	})
	return json.RawMessage(payload), err
}

// AllZooms asks [Renderer.Legend] for every entry rather than the ones that
// apply at one zoom.
const AllZooms = 255

// Legend is the style's declared legend as JSON, or nil when it declares
// none. Pass a zoom to keep only the entries that apply there, or
// [AllZooms] for all of them.
//
// Entries name the node that draws the symbol rather than restating a
// colour, so you lay out the labels and ask the map itself for the
// swatches: render the named node.
func (r *Renderer) Legend(ctx context.Context, zoom uint8) (json.RawMessage, error) {
	code, payload, err := r.withSlots(ctx, func(slots uint32) (int64, error) {
		return r.call1(ctx, "ezu_legend",
			uint64(r.handle), uint64(zoom), uint64(slots), uint64(slots+4))
	})
	if err != nil || code == 0 {
		return nil, err
	}
	return json.RawMessage(payload), nil
}

// Tile is a tile coordinate.
type Tile struct {
	Z uint8 `json:"z"`
	X int64 `json:"x"`
	Y int64 `json:"y"`
}

// SourceTile is the tile you should actually fetch from name in order to
// draw z/x/y.
//
// For a source that declares max-zoom, a request past the ceiling answers
// with the covering ancestor: fetch that, and bind it with
// [Bind.SourceZoom] set to the Z it returns. Below the ceiling, and for a
// source with no ceiling, the answer is the tile itself, and passing its
// zoom as SourceZoom costs nothing.
//
// This exists so the ceiling lives in one place. A host that hard-codes
// each source's maxzoom keeps a second copy of something the style already
// states, and the two drift.
//
// X and Y may be off the grid, which is what walking a neighbourhood hands
// in: X wraps around the antimeridian, so the western neighbour of z/0/y is
// the real tile on the far side of the world. Y does not wrap — there is no
// tile above the north pole — and an out-of-range row comes back unchanged,
// so the fetch for it misses and the stitch clamps that edge, which is what
// the pole should look like.
func (r *Renderer) SourceTile(ctx context.Context, name string, z uint8, x, y int32) (Tile, error) {
	var out Tile
	_, payload, err := r.withSlots(ctx, func(slots uint32) (int64, error) {
		namePtr, err := r.writeBytes(ctx, []byte(name))
		if err != nil {
			return 0, err
		}
		defer r.free(ctx, namePtr, uint32(len(name)))
		return r.call1(ctx, "ezu_source_tile",
			uint64(r.handle), uint64(namePtr), uint64(len(name)),
			uint64(z), uint64(uint32(x)), uint64(uint32(y)),
			uint64(slots), uint64(slots+4))
	})
	if err != nil {
		return out, err
	}
	if err := json.Unmarshal(payload, &out); err != nil {
		return out, fmt.Errorf("ezu: reading the source tile: %w", err)
	}
	return out, nil
}

// Offset is a neighbour tile's position relative to the tile being
// rendered.
type Offset struct{ DX, DY int }

// RequestedNeighborOffsets names the neighbour tiles the active style
// actually asks for from source, never including the centre. An empty
// result means the centre tile is enough.
//
// What comes back depends on the source's kind. A vector source answers
// from the graph: a node that wants a neighbour names it, and cross-tile
// label collision is usually the only thing that does, so the list is often
// empty. A dem or raster source answers from its own neighbor-fetch, which
// defaults to on and means the whole 3×3 — those bind as one stitched
// canvas, and a neighbour left unbound is not a missing collision candidate
// but a pad filled by clamping the tile's own edge, which shows up as a
// seam in whatever samples it.
func (r *Renderer) RequestedNeighborOffsets(ctx context.Context, source string) ([]Offset, error) {
	var pairs [][2]int
	_, payload, err := r.withSlots(ctx, func(slots uint32) (int64, error) {
		namePtr, err := r.writeBytes(ctx, []byte(source))
		if err != nil {
			return 0, err
		}
		defer r.free(ctx, namePtr, uint32(len(source)))
		return r.call1(ctx, "ezu_requested_neighbor_offsets",
			uint64(r.handle), uint64(namePtr), uint64(len(source)),
			uint64(slots), uint64(slots+4))
	})
	if err != nil {
		return nil, err
	}
	if err := json.Unmarshal(payload, &pairs); err != nil {
		return nil, fmt.Errorf("ezu: reading the neighbour offsets: %w", err)
	}
	out := make([]Offset, 0, len(pairs))
	for _, p := range pairs {
		out = append(out, Offset{DX: p[0], DY: p[1]})
	}
	return out, nil
}

// NeededCodepoints names the codepoints the currently bound features can
// require, per glyphs source, sorted ascending.
//
// This is the precise form of [Renderer.NeededGlyphRanges]: a host that can
// build its own glyph PBF — one message holding just these codepoints —
// transfers only the glyphs the tile draws instead of the whole
// 256-codepoint block around each of them. On CJK labels that is the
// difference between a few thousand glyphs and a few tens of megabytes.
// [Renderer.BindSource] files each glyph by its own id, so such a subset
// may span any number of blocks.
//
// It is an over-approximation, deliberately: a codepoint is listed if any
// feature in a text layer carries it in a property the layer's text
// expression reads, without evaluating filters, zoom ranges, or the
// expression itself, and for every fontstack in that layer's fallback
// chain. So it never omits something a label needs, and may name a few that
// go unused.
func (r *Renderer) NeededCodepoints(ctx context.Context) (map[string][]uint32, error) {
	return r.glyphUnits(ctx, "ezu_needed_codepoints")
}

// NeededGlyphRanges names the glyph ranges the currently bound features can
// require, per glyphs source, as range starts (0, 256, 512, …) — the
// {range} in a …/{fontstack}/{range}.pbf URL is <start>-<start+255>.
//
// For hosts that can only fetch whole {range}.pbf files off a MapLibre
// glyphs endpoint. A range holds 256 codepoints and a tile typically draws
// a handful of them, so it is a coarse unit; if you can assemble your own
// subset PBF, use [Renderer.NeededCodepoints] instead. Both see the same
// codepoints and carry the same over-approximation caveat.
//
// This host cannot fetch glyphs lazily, so every range a tile's labels
// touch must be bound before the render.
func (r *Renderer) NeededGlyphRanges(ctx context.Context) (map[string][]uint32, error) {
	return r.glyphUnits(ctx, "ezu_needed_glyph_ranges")
}

func (r *Renderer) glyphUnits(ctx context.Context, export string) (map[string][]uint32, error) {
	out := map[string][]uint32{}
	_, payload, err := r.withSlots(ctx, func(slots uint32) (int64, error) {
		return r.call1(ctx, export, uint64(r.handle), uint64(slots), uint64(slots+4))
	})
	if err != nil {
		return nil, err
	}
	if err := json.Unmarshal(payload, &out); err != nil {
		return nil, fmt.Errorf("ezu: reading %s: %w", export, err)
	}
	return out, nil
}

// Usage is what a renderer is holding, in bytes, so you can shed load
// before an allocation fails rather than after.
//
// These are payload sizes, not an accounting of the heap: they omit
// allocator overhead, decoded features, per-font glyph-path caches, and the
// buffers a render is using right now. Expect the parts to sum to less than
// HeapBytes.
type Usage struct {
	// HeapBytes is wasm linear memory committed to the instance. This is
	// the number a memory limit applies to. It is a high-water mark:
	// freeing Rust values returns them to the allocator, never to the host,
	// so it only ever grows, and only a fresh renderer resets it.
	HeapBytes uint64 `json:"heapBytes"`
	// GlyphBytes are SDF bitmaps resident in the glyph bank, spanning
	// GlyphRanges 256-codepoint blocks. Glyphs accumulate for the life of
	// the renderer and survive ClearSources, so on a long-lived instance
	// this is usually what grew.
	GlyphBytes  uint64 `json:"glyphBytes"`
	GlyphRanges uint64 `json:"glyphRanges"`
	// GlyphBudget is the per-fontstack ceiling [Renderer.SetGlyphBudget]
	// put on them, or nil if none. GlyphBytes totals every fontstack, so it
	// can exceed the budget legitimately.
	GlyphBudget *uint64 `json:"glyphBudget"`
	// FontBytes are outline font files held in the font bank.
	FontBytes uint64 `json:"fontBytes"`
	// ImageBytes are decoded pixels of bound images and sprite atlases.
	ImageBytes uint64 `json:"imageBytes"`
	// CacheBytes is the render cache's pixel payload against its own
	// eviction budget. It bounds itself, so CacheBytes near CacheBudget is
	// steady state, not a leak.
	CacheBytes  uint64 `json:"cacheBytes"`
	CacheBudget uint64 `json:"cacheBudget"`
}

// MemoryUsage reports what this renderer is holding.
func (r *Renderer) MemoryUsage(ctx context.Context) (Usage, error) {
	var out Usage
	_, payload, err := r.withSlots(ctx, func(slots uint32) (int64, error) {
		return r.call1(ctx, "ezu_memory_usage", uint64(r.handle), uint64(slots), uint64(slots+4))
	})
	if err != nil {
		return out, err
	}
	if err := json.Unmarshal(payload, &out); err != nil {
		return out, fmt.Errorf("ezu: reading the memory usage: %w", err)
	}
	return out, nil
}

// RenderTile renders one tile from whatever sources are currently bound and
// returns the encoded bytes.
func (r *Renderer) RenderTile(ctx context.Context, z uint8, x, y uint32, opts Render) ([]byte, error) {
	optsJSON, err := opts.json()
	if err != nil {
		return nil, fmt.Errorf("ezu: encoding the render options: %w", err)
	}
	_, payload, err := r.withSlots(ctx, func(slots uint32) (int64, error) {
		optsPtr, err := r.writeBytes(ctx, optsJSON)
		if err != nil {
			return 0, err
		}
		defer r.free(ctx, optsPtr, uint32(len(optsJSON)))
		return r.call1(ctx, "ezu_render_tile",
			uint64(r.handle), uint64(z), uint64(x), uint64(y),
			uint64(optsPtr), uint64(len(optsJSON)),
			uint64(slots), uint64(slots+4))
	})
	return payload, err
}

// --- module-wide -----------------------------------------------------------

// OpCount is the number of node ops registered in this instance. It is the
// smoke test for the module's life-before-main constructors: zero means
// they never ran, and every style is about to fail with "unknown op".
func (r *Renderer) OpCount(ctx context.Context) (uint32, error) {
	n, err := r.call(ctx, func(ctx context.Context) (int64, error) {
		return r.call1(ctx, "ezu_op_count")
	})
	return uint32(n), err
}

// OpNames lists every registered node op.
func (r *Renderer) OpNames(ctx context.Context) ([]string, error) {
	_, payload, err := r.withSlots(ctx, func(slots uint32) (int64, error) {
		_, err := r.call1(ctx, "ezu_op_names", uint64(slots), uint64(slots+4))
		return 0, err
	})
	if err != nil || len(payload) == 0 {
		return nil, err
	}
	return strings.Split(string(payload), "\n"), nil
}

// SIMDEnabled reports whether the module was compiled with +simd128. The
// committed one is: it is both faster and smaller.
func (r *Renderer) SIMDEnabled(ctx context.Context) (bool, error) {
	n, err := r.call(ctx, func(ctx context.Context) (int64, error) {
		return r.call1(ctx, "ezu_simd_enabled")
	})
	return n != 0, err
}

// HeapBytes is the wasm linear memory currently committed to this
// renderer's instance — the figure a memory limit applies to. It never
// falls; see [Usage.HeapBytes].
func (r *Renderer) HeapBytes(ctx context.Context) (uint64, error) {
	n, err := r.call(ctx, func(ctx context.Context) (int64, error) {
		return r.call1(ctx, "ezu_heap_bytes")
	})
	return uint64(n), err
}

// Log levels for [Renderer.InitLog].
const (
	LogOff = iota
	LogError
	LogWarn
	LogInfo
	LogDebug
	LogTrace
)

// InitLog starts collecting the renderer's log events at the given level.
// Idempotent per instance; the first level wins.
//
// The renderer says things through this channel that are not errors and so
// do not come back from a call: a style whose graph built with warnings, a
// DEM source whose 3×3 was not fully bound so the tile will seam at its
// border, a label whose glyphs were missing and was dropped. Turn it on at
// [LogWarn] and drain after each render if you want to hear them.
func (r *Renderer) InitLog(ctx context.Context, level int) error {
	_, err := r.call(ctx, func(ctx context.Context) (int64, error) {
		return r.call1(ctx, "ezu_log_init", uint64(uint32(level)))
	})
	return err
}

// DrainLogs takes every buffered log line and empties the buffer. Each line
// is "<LEVEL> <target>: <message> k=v …".
//
// There is no timestamp on a line: the module has no clock worth importing,
// and a host that drains right after the call it cares about knows when
// they happened better than the module does.
func (r *Renderer) DrainLogs(ctx context.Context) ([]string, error) {
	_, payload, err := r.withSlots(ctx, func(slots uint32) (int64, error) {
		_, err := r.call1(ctx, "ezu_drain_logs", uint64(slots), uint64(slots+4))
		return 0, err
	})
	if err != nil || len(payload) == 0 {
		return nil, err
	}
	return strings.Split(string(payload), "\n"), nil
}
