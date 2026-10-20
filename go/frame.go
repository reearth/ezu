package ezu

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"image"
	"math"
)

// A printed map is asked for as "1:1000 at 200 dpi, 245 × 174 mm, centred
// here", not as a zoom level. The renderer only draws whole tiles at integer
// zooms, so the scale is reached through the tile size instead: at zoom z a
// tile of T px spans 1/2^z of the world, and choosing T sets how many ground
// metres one output pixel covers. [Renderer.ComputeFrame] picks z and T and
// places the frame on that pixel grid; [Renderer.RenderFrame] renders the
// tiles it covers and crops them to the frame. Together they produce what
// `ezu frame` writes for the same style and data, pixel for pixel.

// FrameRequest is what a print frame is asked for.
type FrameRequest struct {
	// CenterLon and CenterLat are the frame's centre in degrees (WGS84). The
	// scale is matched at this latitude; Web Mercator stretches it away from
	// it.
	CenterLon, CenterLat float64
	// Scale is the scale denominator: 1000 for 1:1000.
	Scale float64
	// DPI is the output resolution. Together with Scale it fixes how many
	// ground metres one output pixel covers.
	DPI float64
	// Width and Height are the output size in pixels. Paper sizes convert
	// with [MillimetresToPixels].
	Width, Height uint32
	// StyleTileSize is the tile size the default zoom aims at: a style is
	// tuned for its own tile size (zoom-dependent widths, label density), so
	// the zoom chosen keeps the rendered tile size as close to it as the
	// scale allows. Zero takes this renderer's style, which is what you want
	// unless you are computing frames for a style the renderer has not
	// loaded.
	StyleTileSize uint32
}

// FrameOption qualifies one call to [Renderer.ComputeFrame]. With none, the
// zoom is chosen for you.
type FrameOption func(*frameOptions)

type frameOptions struct {
	zoom    int32
	hasZoom bool
}

// AtZoom renders the frame at zoom z, deriving the tile size from it,
// rather than choosing the zoom whose tile size is nearest the style's — the
// CLI's --zoom.
//
// An option rather than a field because zoom 0 is a real zoom, so a zero
// cannot also mean "choose one".
func AtZoom(z int32) FrameOption {
	return func(o *frameOptions) { o.zoom, o.hasZoom = z, true }
}

// TileRange is a run of tile columns or rows, Start inclusive and End
// exclusive.
type TileRange struct{ Start, End int32 }

// Len is the number of tiles in the run.
func (r TileRange) Len() int {
	if r.End < r.Start {
		return 0
	}
	return int(r.End - r.Start)
}

// Frame is where a print frame sits on the tile grid it renders from.
type Frame struct {
	// Zoom is the zoom the tiles render at.
	Zoom int32
	// TileSize is the tile size, in pixels, the tiles render at. It is a
	// whole pixel, so the scale is met only as closely as Scale says.
	TileSize uint32
	// Scale is the scale denominator this zoom and tile size actually
	// produce at the centre latitude.
	Scale float64
	// Width and Height are the output size in pixels, as requested.
	Width, Height uint32
	// Left and Top are the frame's top-left corner in world pixels — the
	// pixel grid of the whole world at this zoom and tile size, TileSize ·
	// 2^Zoom wide — rounded to a whole pixel so tiles land on the frame
	// without resampling.
	Left, Top int64
	// ShiftX and ShiftY are how far that rounding moved the frame, in
	// pixels: the requested centre sits at (Width/2 + ShiftX, Height/2 +
	// ShiftY) in the output. Each is within ±0.5.
	ShiftX, ShiftY float64
	// TilesX and TilesY are the tile columns and rows the frame touches.
	// Rows are clamped to the world, so a frame reaching past the Mercator
	// edge leaves that part of the image empty.
	TilesX, TilesY TileRange
}

// TileCount is the number of tiles the frame renders.
func (f Frame) TileCount() int { return f.TilesX.Len() * f.TilesY.Len() }

// Tiles lists every tile the frame renders, row by row from the top, which
// is the order [Renderer.RenderFrame] renders them in.
func (f Frame) Tiles() []Tile {
	out := make([]Tile, 0, f.TileCount())
	for y := f.TilesY.Start; y < f.TilesY.End; y++ {
		for x := f.TilesX.Start; x < f.TilesX.End; x++ {
			out = append(out, Tile{Z: f.Zoom, X: x, Y: y})
		}
	}
	return out
}

// TileOffset is where tile's top-left corner lands in the output image, in
// pixels. It is negative, or past the frame, for a tile along the border
// that only partly overlaps it.
func (f Frame) TileOffset(tile Tile) (dx, dy int64) {
	t := int64(f.TileSize)
	return int64(tile.X)*t - f.Left, int64(tile.Y)*t - f.Top
}

// MillimetresToPixels converts a paper length to whole pixels at dpi, the
// way `ezu frame --size-mm` does.
func MillimetresToPixels(mm, dpi float64) uint32 {
	return uint32(math.Round(mm / 25.4 * dpi))
}

// frameWire is the module's spelling of a frame.
type frameWire struct {
	Zoom     int32      `json:"zoom"`
	TileSize uint32     `json:"tileSize"`
	Scale    float64    `json:"scale"`
	Width    uint32     `json:"width"`
	Height   uint32     `json:"height"`
	Left     int64      `json:"left"`
	Top      int64      `json:"top"`
	Shift    [2]float64 `json:"shift"`
	TilesX   [2]int32   `json:"tilesX"`
	TilesY   [2]int32   `json:"tilesY"`
}

// ComputeFrame works out the zoom, tile size and pixel placement that render
// req: the arithmetic `ezu frame` does before it renders anything, done by
// the same code, so a frame computed here is the one the CLI would render.
//
// It does not change the renderer. Render the frame with
// [Renderer.RenderFrame], or tile by tile at Frame.TileSize — through
// [Renderer.SetTileSize] or [Render.TileSize].
//
// A request out of its domain (a non-positive scale, a latitude past the
// Mercator edge, a zero-sized output), a frame across the antimeridian, and a
// scale that would need a tile size outside 16..8192 px all fail with
// [KindInvalidStyle].
func (r *Renderer) ComputeFrame(ctx context.Context, req FrameRequest, opts ...FrameOption) (Frame, error) {
	var o frameOptions
	for _, opt := range opts {
		opt(&o)
	}
	payload := map[string]any{
		"centerLon": req.CenterLon,
		"centerLat": req.CenterLat,
		"scale":     req.Scale,
		"dpi":       req.DPI,
		"width":     req.Width,
		"height":    req.Height,
	}
	if req.StyleTileSize != 0 {
		payload["styleTileSize"] = req.StyleTileSize
	}
	if o.hasZoom {
		payload["zoom"] = o.zoom
	}
	reqJSON, err := json.Marshal(payload)
	if err != nil {
		return Frame{}, fmt.Errorf("ezu: encoding the frame request: %w", err)
	}
	_, reply, err := r.withSlots(ctx, func(slots uint32) (int64, error) {
		ptr, err := r.writeBytes(ctx, reqJSON)
		if err != nil {
			return 0, err
		}
		defer r.free(ctx, ptr, uint32(len(reqJSON)))
		return r.call1(ctx, "ezu_compute_frame",
			uint64(r.handle), uint64(ptr), uint64(len(reqJSON)),
			uint64(slots), uint64(slots+4))
	})
	if err != nil {
		return Frame{}, err
	}
	var w frameWire
	if err := json.Unmarshal(reply, &w); err != nil {
		return Frame{}, fmt.Errorf("ezu: reading the frame: %w", err)
	}
	return Frame{
		Zoom:     w.Zoom,
		TileSize: w.TileSize,
		Scale:    w.Scale,
		Width:    w.Width,
		Height:   w.Height,
		Left:     w.Left,
		Top:      w.Top,
		ShiftX:   w.Shift[0],
		ShiftY:   w.Shift[1],
		TilesX:   TileRange{Start: w.TilesX[0], End: w.TilesX[1]},
		TilesY:   TileRange{Start: w.TilesY[0], End: w.TilesY[1]},
	}, nil
}

// SetTileSize renders every tile at size px instead of the style's
// tile-size, without editing the style. [Renderer.ClearTileSize] goes back to
// the style's, and so does [Renderer.SetStyle].
//
// A print frame needs it: [Renderer.ComputeFrame] meets the scale by
// choosing the tile size, and the frame's tiles must be drawn at exactly
// that size. It renders what the style with its tile-size edited would.
// Nothing you bind depends on it — [Renderer.RequestedNeighborOffsets] and
// [Renderer.SourceTile] answer as before — and the renderer's intermediate
// cache, which is sized by the tile, is dropped when the size changes.
// [Renderer.TileSize] keeps reporting the style's own. A [Render.TileSize] on
// one render still wins over this.
//
// Size 0 is refused rather than read as "the style's": that is
// ClearTileSize, and spelling it with a zero would let an unset field
// quietly undo the setting.
func (r *Renderer) SetTileSize(ctx context.Context, size uint32) error {
	if size == 0 {
		return errors.New("ezu: a tile size must be at least 1 px; ClearTileSize goes back to the style's")
	}
	if err := r.idle(); err != nil {
		return err
	}
	return r.setTileSize(ctx, size)
}

// ClearTileSize goes back to rendering at the style's own tile-size, undoing
// [Renderer.SetTileSize].
func (r *Renderer) ClearTileSize(ctx context.Context) error {
	if err := r.idle(); err != nil {
		return err
	}
	return r.setTileSize(ctx, 0)
}

func (r *Renderer) setTileSize(ctx context.Context, size uint32) error {
	_, err := r.call(ctx, func(ctx context.Context) (int64, error) {
		return r.call1(ctx, "ezu_set_tile_size", uint64(r.handle), uint64(size))
	})
	return err
}

// RenderFrame renders every tile f covers and copies each one's share into a
// frame-sized image, which is what `ezu frame` does: the same tiles, at the
// same tile size, cropped the same way. The image is straight
// (un-premultiplied) RGBA, as [FormatRGBA] is, so it encodes to the PNG the
// CLI writes.
//
// The tile data stays yours to fetch, tile by tile, but it is handed over as
// a value rather than bound: before each tile RenderFrame calls sources with
// the tile about to render, and renders the tile from the [SourceSet] it
// returns, as [Renderer.RenderTileWith] does — the tile itself, the
// neighbours [Renderer.RequestedNeighborOffsets] names, the ancestor
// [Renderer.SourceTile] answers past a source's max-zoom. A frame's tiles
// border each other, so keep the [SourceData] that [Renderer.LoadSource]
// gives you and put each one into the set of every tile that reads it: each
// fetched tile is then decoded once, not once per tile around it. Release
// them when the frame returns.
//
// The [FrameBinder] sources is handed binds into the renderer what a set
// turns out to need — the glyphs [Renderer.NeededCodepointsIn] lists for it.
// A sources that returns an error stops the frame with that error. sources
// may be nil for a style that needs no tile data. Persistent sources (fonts,
// sprites, brushes, images) can equally be bound once, before the call.
// Nothing bound with [Renderer.BindSource] that is tile-scoped is read, and
// RenderFrame binds nothing itself. After each tile it trims the glyph banks
// to [Renderer.SetGlyphBudget]'s budget, as [Renderer.RenderTile] does.
//
// Until RenderFrame returns, the renderer is the frame's. Everything that
// would change what it draws — [Renderer.BindSource],
// [Renderer.ClearSources], [Renderer.RenderTile], [Renderer.TrimGlyphs],
// another RenderFrame or [Renderer.RenderFrameWithFeatures],
// [Renderer.SetStyle], [Renderer.SetTileSize],
// [Renderer.ClearTileSize], [Renderer.SetGlyphBudget],
// [Renderer.ClearGlyphBudget], [Renderer.SetCacheBudget],
// [Renderer.ClearCacheBudget] — fails with [ErrFrameInProgress], so a
// goroutine that reaches for the renderer while sources is fetching cannot
// change the frame's tiles under it. Loading, releasing and rendering from a
// set, and queries that only read, which sources needs, still work.
//
// The tiles render at f.TileSize through [Render.TileSize], so the renderer's
// own tile size is left as it was. opts carries the rest of a render — Pad,
// Params — and its Format and TileSize must be left zero (or match), since
// the frame fixes both.
//
// Tiles render one after another on this renderer, so a frame takes
// TileCount renders' time. Rendering several frames at once means several
// renderers, as ever; a [Pool] hands them out, and the data each frame loads
// belongs to the renderer it was loaded on.
func (r *Renderer) RenderFrame(ctx context.Context, f Frame, sources func(ctx context.Context, tile Tile, b *FrameBinder) (SourceSet, error), opts Render) (*image.NRGBA, error) {
	opts, err := frameRenderOptions(f, opts)
	if err != nil {
		return nil, err
	}
	if !r.framing.CompareAndSwap(false, true) {
		return nil, ErrFrameInProgress
	}
	defer r.endFrame()
	img, err := r.renderFrameTiles(ctx, f, sources, opts, 0)
	if err != nil {
		return nil, err
	}
	return img, nil
}

// endFrame gives the renderer back after a frame, however the frame ended.
// It runs deferred so that a sources callback that panics, and a caller
// that recovers, still find the renderer free rather than refusing
// everything with [ErrFrameInProgress] for the rest of its life.
func (r *Renderer) endFrame() {
	r.openTile.Store(0)
	r.framing.Store(false)
}

// frameRenderOptions checks opts against the frame and fixes what the frame
// fixes: raw pixels, at the frame's tile size.
func frameRenderOptions(f Frame, opts Render) (Render, error) {
	if f.TileSize == 0 || f.Width == 0 || f.Height == 0 {
		return opts, errors.New("ezu: the frame has no size; compute it with ComputeFrame")
	}
	if opts.Format != "" && opts.Format != FormatRGBA {
		return opts, fmt.Errorf("ezu: a frame renders as raw pixels; leave Render.Format empty, not %q", opts.Format)
	}
	if opts.TileSize != 0 && opts.TileSize != f.TileSize {
		return opts, fmt.Errorf("ezu: the frame renders at tile size %d; leave Render.TileSize zero, not %d",
			f.TileSize, opts.TileSize)
	}
	opts.Format = FormatRGBA
	opts.TileSize = f.TileSize
	return opts, nil
}

// FeatureCoords is the coordinates [Renderer.RenderFrameWithFeatures] writes
// a page's features in.
type FeatureCoords string

const (
	// CoordsLonLat is WGS84 longitude and latitude, as RFC 7946 GeoJSON
	// expects, to seven decimal places. It is the default.
	CoordsLonLat FeatureCoords = "lonlat"
	// CoordsPx is pixels of the page: origin at its top-left corner, y down,
	// to two decimal places.
	CoordsPx FeatureCoords = "px"
)

// FrameFeatures is which features nodes [Renderer.RenderFrameWithFeatures]
// gathers over a page, and how it writes them.
type FrameFeatures struct {
	// Nodes is one page of features per entry, in this order. A node may be
	// named more than once, with different keys.
	Nodes []FrameFeatureNode
	// Coords is the coordinates the features are written in; empty is
	// [CoordsLonLat].
	Coords FeatureCoords
}

// FrameFeatureNode is one features node to gather over a page.
type FrameFeatureNode struct {
	// Node is the node's id in the style.
	Node string
	// Key names the properties that identify a feature across tiles —
	// "label-id" and "part" for label-boxes. With a key, the features whose
	// key properties are all equal are written once, the first in reading
	// order, and the page is sorted by the key. With none (nil), every
	// tile's features are written, so a feature several tiles see appears
	// once per tile, and a shape cut at tile edges appears as its pieces.
	Key []string
}

// PageFeatures is what one features node held over a whole page, as
// [Renderer.RenderFrameWithFeatures] hands it back.
type PageFeatures struct {
	// Node is the node's id, as it was asked for.
	Node string
	// GeoJSON is a FeatureCollection with one Feature per line, byte for
	// byte the file `ezu frame --features` writes for the same style, data
	// and frame. Features wholly off the page are left out, and one that
	// reaches into it is written whole, not clipped. Rings are closed and
	// wound as RFC 7946 asks, as drawn on the map.
	GeoJSON []byte
}

// RenderFrameWithFeatures is [Renderer.RenderFrame], also handing back what
// the features nodes features names held over the whole page: one
// [PageFeatures] per entry of features.Nodes, in that order. It is what
// `ezu frame --features` writes, gathered by the same code.
//
// The values are the ones each tile's render computed on its way to the
// pixels, so this costs no second evaluation, and the image is the one
// RenderFrame returns. Tiles are read rows top to bottom, then left to
// right, and each tile's features in their own order; with a key the first
// copy in that order is the one kept.
//
// A name that is not a node of the style, or names one that produces
// something other than features, and a key that names no property, fail
// with [KindInvalidStyle] before anything renders.
func (r *Renderer) RenderFrameWithFeatures(ctx context.Context, f Frame, sources func(ctx context.Context, tile Tile, b *FrameBinder) (SourceSet, error), features FrameFeatures, opts Render) (*image.NRGBA, []PageFeatures, error) {
	opts, err := frameRenderOptions(f, opts)
	if err != nil {
		return nil, nil, err
	}
	reqJSON, err := frameFeaturesJSON(f, features)
	if err != nil {
		return nil, nil, err
	}
	if !r.framing.CompareAndSwap(false, true) {
		return nil, nil, ErrFrameInProgress
	}
	defer r.endFrame()
	id, err := r.call(ctx, func(ctx context.Context) (int64, error) {
		ptr, err := r.writeBytes(ctx, reqJSON)
		if err != nil {
			return 0, err
		}
		defer r.free(ctx, ptr, uint32(len(reqJSON)))
		return r.call1(ctx, "ezu_frame_features_new", uint64(r.handle), uint64(ptr), uint64(len(reqJSON)))
	})
	if err != nil {
		return nil, nil, err
	}
	// Until finish takes it, the gathering is dropped with the frame,
	// however the frame ends — a sources callback that panics included; a
	// renderer that died took it along.
	finished := false
	defer func() {
		if !finished {
			_, _ = r.call(ctx, func(ctx context.Context) (int64, error) {
				return r.call1(ctx, "ezu_frame_features_free", uint64(r.handle), uint64(id))
			})
		}
	}()
	img, err := r.renderFrameTiles(ctx, f, sources, opts, uint32(id))
	if err != nil {
		return nil, nil, err
	}
	finished = true
	_, reply, err := r.withSlots(ctx, func(slots uint32) (int64, error) {
		return r.call1(ctx, "ezu_frame_features_finish", uint64(r.handle), uint64(id),
			uint64(slots), uint64(slots+4))
	})
	if err != nil {
		return nil, nil, err
	}
	var wire []struct {
		Node    string `json:"node"`
		GeoJSON string `json:"geojson"`
	}
	if err := json.Unmarshal(reply, &wire); err != nil {
		return nil, nil, fmt.Errorf("ezu: reading the page features: %w", err)
	}
	out := make([]PageFeatures, len(wire))
	for i, p := range wire {
		out[i] = PageFeatures{Node: p.Node, GeoJSON: []byte(p.GeoJSON)}
	}
	return img, out, nil
}

// frameFeaturesJSON is the module's spelling of a frame features request.
func frameFeaturesJSON(f Frame, features FrameFeatures) ([]byte, error) {
	type node struct {
		Node string   `json:"node"`
		Key  []string `json:"key,omitempty"`
	}
	nodes := make([]node, len(features.Nodes))
	for i, n := range features.Nodes {
		if n.Key != nil && len(n.Key) == 0 {
			return nil, &Error{
				Name:    KindInvalidStyle,
				Message: fmt.Sprintf("the key for node `%s` must name one or more properties", n.Node),
				Code:    -1,
			}
		}
		nodes[i] = node{Node: n.Node, Key: n.Key}
	}
	coords := features.Coords
	if coords == "" {
		coords = CoordsLonLat
	}
	b, err := json.Marshal(map[string]any{
		"frame": frameWire{
			Zoom:     f.Zoom,
			TileSize: f.TileSize,
			Scale:    f.Scale,
			Width:    f.Width,
			Height:   f.Height,
			Left:     f.Left,
			Top:      f.Top,
			Shift:    [2]float64{f.ShiftX, f.ShiftY},
			TilesX:   [2]int32{f.TilesX.Start, f.TilesX.End},
			TilesY:   [2]int32{f.TilesY.Start, f.TilesY.End},
		},
		"nodes":  nodes,
		"coords": coords,
	})
	if err != nil {
		return nil, fmt.Errorf("ezu: encoding the frame features request: %w", err)
	}
	return b, nil
}

// renderFrameTiles renders f's tiles into one image. A non-zero gathering
// renders each tile through it, so it keeps what its nodes held.
func (r *Renderer) renderFrameTiles(ctx context.Context, f Frame, sources func(ctx context.Context, tile Tile, b *FrameBinder) (SourceSet, error), opts Render, gathering uint32) (*image.NRGBA, error) {
	img := image.NewNRGBA(image.Rect(0, 0, int(f.Width), int(f.Height)))
	t := int(f.TileSize)
	var tileScoped map[string]bool
	if sources != nil {
		declared, err := r.Sources(ctx)
		if err != nil {
			return nil, err
		}
		tileScoped = make(map[string]bool, len(declared))
		for _, s := range declared {
			tileScoped[s.Name] = s.TileScoped
		}
	}
	for _, tile := range f.Tiles() {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		var set SourceSet
		if sources != nil {
			b := &FrameBinder{r: r, tile: r.issued.Add(1), tileScoped: tileScoped}
			var err error
			set, err = r.tileSources(ctx, sources, tile, b)
			if err != nil {
				return nil, fmt.Errorf("ezu: sources for tile %d/%d/%d: %w", tile.Z, tile.X, tile.Y, err)
			}
		}
		var px []byte
		var err error
		if gathering == 0 {
			px, err = r.RenderTileWith(ctx, tile, set, opts)
		} else {
			px, err = r.renderGatheredTile(ctx, gathering, tile, set, opts)
		}
		// Trimmed whether or not the tile rendered, as RenderTile does: the
		// glyphs were bound for it either way.
		if trimErr := r.trimGlyphs(ctx); err == nil {
			err = trimErr
		}
		if err != nil {
			return nil, err
		}
		if len(px) != t*t*4 {
			return nil, fmt.Errorf("ezu: tile %d/%d/%d came back as %d bytes, want %d for %dx%d",
				tile.Z, tile.X, tile.Y, len(px), t*t*4, t, t)
		}
		dx, dy := f.TileOffset(tile)
		blitTile(img, px, t, int(dx), int(dy))
	}
	return img, nil
}

// tileSources runs one tile's sources with b open, and closes b however
// sources returns, so a binder kept from a callback that panicked is as
// stale as any other.
func (r *Renderer) tileSources(ctx context.Context, sources func(ctx context.Context, tile Tile, b *FrameBinder) (SourceSet, error), tile Tile, b *FrameBinder) (SourceSet, error) {
	r.openTile.Store(b.tile)
	defer r.openTile.Store(0)
	return sources(ctx, tile, b)
}

// renderGatheredTile is [Renderer.RenderTileWith] for one tile of a frame
// whose features the module is gathering under id.
func (r *Renderer) renderGatheredTile(ctx context.Context, id uint32, tile Tile, set SourceSet, opts Render) ([]byte, error) {
	setJSON, err := set.json(r)
	if err != nil {
		return nil, err
	}
	optsJSON, err := opts.json()
	if err != nil {
		return nil, fmt.Errorf("ezu: encoding the render options: %w", err)
	}
	_, payload, err := r.withSlots(ctx, func(slots uint32) (int64, error) {
		setPtr, err := r.writeBytes(ctx, setJSON)
		if err != nil {
			return 0, err
		}
		defer r.free(ctx, setPtr, uint32(len(setJSON)))
		optsPtr, err := r.writeBytes(ctx, optsJSON)
		if err != nil {
			return 0, err
		}
		defer r.free(ctx, optsPtr, uint32(len(optsJSON)))
		return r.call1(ctx, "ezu_frame_features_render_tile",
			uint64(r.handle), uint64(id),
			uint64(uint32(tile.Z)), uint64(uint32(tile.X)), uint64(uint32(tile.Y)),
			uint64(setPtr), uint64(len(setJSON)),
			uint64(optsPtr), uint64(len(optsJSON)),
			uint64(slots), uint64(slots+4))
	})
	return payload, err
}

// FrameBinder is what [Renderer.RenderFrame] hands each tile's sources: the
// way to bind into the renderer — the glyphs a tile turns out to need, or a
// font, sprite, brush or image — while the frame holds it.
//
// It binds for its own tile only, and only until that tile's sources
// returns; kept and used later it fails with [ErrStaleBinder].
type FrameBinder struct {
	r          *Renderer
	tile       uint64
	tileScoped map[string]bool
}

// BindSource is [Renderer.BindSource] for the sources bound into the
// renderer itself. A tile-scoped source is refused with [KindUnknownSrc]:
// the frame renders each tile from the set sources returns and never reads
// what is bound, so tile data bound here would be dropped without a word.
// Load it and put it in the set instead.
func (b *FrameBinder) BindSource(ctx context.Context, name string, data []byte, opts ...BindOption) error {
	if b.r.openTile.Load() != b.tile {
		return ErrStaleBinder
	}
	if b.tileScoped[name] {
		return &Error{
			Name:    KindUnknownSrc,
			Message: fmt.Sprintf("source `%s` is tile-scoped; load it and put it in the set the tile's sources returns", name),
			Code:    -13,
		}
	}
	return b.r.bindSource(ctx, name, data, opts...)
}

// ErrFrameInProgress is returned by a call that would change a renderer
// while [Renderer.RenderFrame] holds it. Bind what a frame's tiles need
// through the [FrameBinder] it passes, and wait for it to return for
// anything else.
var ErrFrameInProgress = errors.New("ezu: the renderer is rendering a frame; bind what its tiles need through the FrameBinder RenderFrame passes, and wait for it to return for anything else")

// ErrStaleBinder is returned by a [FrameBinder] used after its tile's
// sources has returned.
var ErrStaleBinder = errors.New("ezu: this FrameBinder's tile is no longer being rendered; bind only from inside that tile's sources")

// blitTile copies a t×t straight-RGBA tile into dst with its top-left at
// (dx, dy), keeping only the part inside dst. Rows are copied, not
// composited: tiles never overlap, so there is nothing underneath to blend
// with, and dst gets exactly the tile's pixels.
func blitTile(dst *image.NRGBA, tile []byte, t, dx, dy int) {
	b := dst.Bounds()
	x0, x1 := max(dx, b.Min.X), min(dx+t, b.Max.X)
	y0, y1 := max(dy, b.Min.Y), min(dy+t, b.Max.Y)
	if x0 >= x1 || y0 >= y1 {
		return
	}
	row := (x1 - x0) * 4
	for y := y0; y < y1; y++ {
		src := ((y-dy)*t + (x0 - dx)) * 4
		off := dst.PixOffset(x0, y)
		copy(dst.Pix[off:off+row], tile[src:src+row])
	}
}
