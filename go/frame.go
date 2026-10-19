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
// Binding stays yours, tile by tile, as for [Renderer.RenderTile], but it
// goes through the [FrameBinder] the frame hands you. Before each tile
// RenderFrame clears the tile-scoped sources and calls bind with the tile
// about to render; bind fetches what that tile needs — the tile itself, the
// neighbours [Renderer.RequestedNeighborOffsets] names, the ancestor
// [Renderer.SourceTile] answers past a source's max-zoom, the glyphs
// [Renderer.NeededCodepoints] lists — and binds it with
// [FrameBinder.BindSource]. A bind that returns an error stops the frame with
// that error. bind may be nil for a style that needs nothing bound per tile.
// Persistent sources (fonts, sprites, brushes, images) are bound once,
// before the call, as usual.
//
// Until RenderFrame returns, the renderer is the frame's. Everything that
// would change what it draws — [Renderer.BindSource],
// [Renderer.ClearSources], [Renderer.RenderTile], another RenderFrame,
// [Renderer.SetStyle], [Renderer.SetTileSize], [Renderer.ClearTileSize],
// [Renderer.SetGlyphBudget], [Renderer.ClearGlyphBudget] — fails with
// [ErrFrameInProgress], so a goroutine that reaches for the renderer while
// bind is fetching cannot land its bindings in the frame's tiles. Queries
// that only read, which bind needs, still answer. However the frame ends,
// the renderer is given back with nothing of its tiles bound.
//
// The tiles render at f.TileSize through [Render.TileSize], so the renderer's
// own tile size is left as it was. opts carries the rest of a render — Pad,
// Params — and its Format and TileSize must be left zero (or match), since
// the frame fixes both.
//
// Tiles render one after another on this renderer, so a frame takes
// TileCount renders' time. Rendering several frames at once means several
// renderers, as ever; a [Pool] hands them out.
func (r *Renderer) RenderFrame(ctx context.Context, f Frame, bind func(ctx context.Context, tile Tile, b *FrameBinder) error, opts Render) (*image.NRGBA, error) {
	if f.TileSize == 0 || f.Width == 0 || f.Height == 0 {
		return nil, errors.New("ezu: the frame has no size; compute it with ComputeFrame")
	}
	if opts.Format != "" && opts.Format != FormatRGBA {
		return nil, fmt.Errorf("ezu: a frame renders as raw pixels; leave Render.Format empty, not %q", opts.Format)
	}
	if opts.TileSize != 0 && opts.TileSize != f.TileSize {
		return nil, fmt.Errorf("ezu: the frame renders at tile size %d; leave Render.TileSize zero, not %d",
			f.TileSize, opts.TileSize)
	}
	opts.Format = FormatRGBA
	opts.TileSize = f.TileSize

	if !r.framing.CompareAndSwap(false, true) {
		return nil, ErrFrameInProgress
	}
	img, err := r.renderFrameTiles(ctx, f, bind, opts)
	r.openTile.Store(0)
	// Leave nothing of the last tile bound, as a tile loop would, whether
	// or not the frame made it.
	if clearErr := r.clearSources(ctx); err == nil {
		err = clearErr
	}
	r.framing.Store(false)
	if err != nil {
		return nil, err
	}
	return img, nil
}

func (r *Renderer) renderFrameTiles(ctx context.Context, f Frame, bind func(ctx context.Context, tile Tile, b *FrameBinder) error, opts Render) (*image.NRGBA, error) {
	img := image.NewNRGBA(image.Rect(0, 0, int(f.Width), int(f.Height)))
	t := int(f.TileSize)
	for _, tile := range f.Tiles() {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		if err := r.clearSources(ctx); err != nil {
			return nil, err
		}
		if bind != nil {
			b := &FrameBinder{r: r, tile: r.issued.Add(1)}
			r.openTile.Store(b.tile)
			err := bind(ctx, tile, b)
			r.openTile.Store(0)
			if err != nil {
				return nil, fmt.Errorf("ezu: binding tile %d/%d/%d: %w", tile.Z, tile.X, tile.Y, err)
			}
		}
		px, err := r.renderTile(ctx, tile, opts)
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

// FrameBinder is what [Renderer.RenderFrame] hands each tile's bind: the way
// to bind sources while the frame holds the renderer.
//
// It binds for its own tile only, and only until that tile's bind returns;
// kept and used later it fails with [ErrStaleBinder].
type FrameBinder struct {
	r    *Renderer
	tile uint64
}

// BindSource is [Renderer.BindSource] for the tile this binder was handed
// with.
func (b *FrameBinder) BindSource(ctx context.Context, name string, data []byte, opts ...BindOption) error {
	if b.r.openTile.Load() != b.tile {
		return ErrStaleBinder
	}
	return b.r.bindSource(ctx, name, data, opts...)
}

// ErrFrameInProgress is returned by a call that would change a renderer
// while [Renderer.RenderFrame] holds it. Bind the frame's tiles through the
// [FrameBinder] it passes, and wait for it to return for anything else.
var ErrFrameInProgress = errors.New("ezu: the renderer is rendering a frame; bind its tiles through the FrameBinder RenderFrame passes, and wait for it to return for anything else")

// ErrStaleBinder is returned by a [FrameBinder] used after its tile's bind
// has returned.
var ErrStaleBinder = errors.New("ezu: this FrameBinder's tile is no longer being bound; bind only from inside that tile's bind")

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
