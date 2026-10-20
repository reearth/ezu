package ezu

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sort"
	"sync/atomic"
)

// Binding is state: bind a tile's bytes, render, clear, bind the next. That
// is a reset every host has to remember, and it decodes a payload each time
// it is bound, so a vector tile that is also its neighbours' neighbour is
// decoded up to nine times over a block of tiles. The types here take the
// table apart: [Renderer.LoadSource] decodes a payload once into a
// [SourceData], a [SourceSet] says which payloads one render reads, and
// [Renderer.RenderTileWith] renders from a set and nothing else that is
// tile-scoped.

// SourceData is one tile-scoped payload — a vector tile, a DEM or imagery
// tile, a GeoJSON document — decoded once by [Renderer.LoadSource] and
// ready to sit in any number of [SourceSet]s.
//
// It lives in the wasm instance of the renderer that loaded it, so it
// belongs to that renderer: a set holding it renders on that renderer and
// fails with [ErrForeignSource] on any other. It stays until
// [SourceData.Release], or until the renderer is closed, which releases
// everything it loaded.
type SourceData struct {
	r    *Renderer
	name string
	// The module's id for it, or 0 once released.
	id atomic.Uint32
}

// Source is the sources.<name> entry the data was loaded for.
func (d *SourceData) Source() string { return d.name }

// Release drops the decoded payload from the renderer's memory. A set that
// still holds it fails with [ErrSourceReleased] when it is rendered from.
// Releasing twice does nothing.
func (d *SourceData) Release(ctx context.Context) error {
	id := d.id.Swap(0)
	if id == 0 {
		return nil
	}
	_, err := d.r.call(ctx, func(ctx context.Context) (int64, error) {
		return d.r.call1(ctx, "ezu_release_source", uint64(d.r.handle), uint64(id))
	})
	return err
}

// ErrSourceReleased is returned for a [SourceData] used after
// [SourceData.Release].
var ErrSourceReleased = errors.New("ezu: this SourceData has been released")

// ErrForeignSource is returned for a [SourceSet] rendered on a renderer
// other than the one that loaded its data. Each renderer is its own wasm
// instance, and the data lives in the one that decoded it; load it on the
// renderer that renders with it.
var ErrForeignSource = errors.New("ezu: this SourceData was loaded by another renderer; load it on the renderer that renders with it")

// LoadSource decodes a tile-scoped source's bytes — mvt, pmtiles, dem,
// raster, or a remote geojson document — once, into a [SourceData] that any
// number of [SourceSet]s can hold without decoding it again.
//
// A host rendering a block of tiles loads each tile it fetched once and
// binds it into the set of every tile it borders, rather than decoding it
// as the centre of one tile and again as a neighbour of each of the others.
// Nothing about what the renderer draws changes, so this works while
// [Renderer.RenderFrame] holds the renderer, which is where a frame's
// callback loads its tiles.
//
// It fails with [KindUnknownSrc] if name is not declared, or is a kind bound
// into the renderer itself with [Renderer.BindSource] — brush, image,
// sprite, font, glyphs — and with the matching decode kind if the bytes do
// not parse. Unlike BindSource, a DEM or imagery tile is decoded here, so
// its decode error comes back here and not from the render.
func (r *Renderer) LoadSource(ctx context.Context, name string, data []byte) (*SourceData, error) {
	id, err := r.call(ctx, func(ctx context.Context) (int64, error) {
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
		return r.call1(ctx, "ezu_load_source",
			uint64(r.handle),
			uint64(namePtr), uint64(len(name)),
			uint64(dataPtr), uint64(len(data)))
	})
	if err != nil {
		return nil, err
	}
	d := &SourceData{r: r, name: name}
	d.id.Store(uint32(id))
	return d, nil
}

// SourceSet is every tile-scoped payload one render reads, each at its
// place in the 3×3 neighbourhood. It is the binding table
// [Renderer.BindSource] fills, as a value you build per tile and hand to
// [Renderer.RenderTileWith].
//
// The zero value is an empty set, ready to use. Like a map, a copy shares
// its entries with the original, so build a fresh one per tile. A set says
// nothing about the tile it is for — offsets are relative — so pairing each
// tile with its neighbourhood is yours to do.
//
// Fonts, sprites, brushes, images and glyphs are not here: they belong to
// the renderer, bound once with BindSource and read by every render.
type SourceSet struct {
	entries map[setKey]setEntry
}

type setKey struct {
	source string
	at     Offset
}

type setEntry struct {
	data          *SourceData
	sourceZoom    int32
	hasSourceZoom bool
}

// Bind puts data into the set at [AtOffset]'s position for its source,
// replacing what was there. [FromZoom] says the data is an ancestor's, to be
// resampled or reprojected into the tile, exactly as for BindSource;
// [WithSpriteIndex] means nothing to tile data and is ignored.
//
// It fails with [ErrSourceReleased] for released data. Two kinds of data
// under one source name are refused, with [KindUnknownSrc], by the render.
func (s *SourceSet) Bind(data *SourceData, opts ...BindOption) error {
	if data == nil || data.id.Load() == 0 {
		return ErrSourceReleased
	}
	var cfg bindOptions
	for _, opt := range opts {
		opt(&cfg)
	}
	if s.entries == nil {
		s.entries = map[setKey]setEntry{}
	}
	s.entries[setKey{source: data.name, at: cfg.offset}] = setEntry{
		data:          data,
		sourceZoom:    cfg.sourceZoom,
		hasSourceZoom: cfg.hasSourceZoom,
	}
	return nil
}

// Contains reports whether anything is bound for source.
func (s SourceSet) Contains(source string) bool {
	for k := range s.entries {
		if k.source == source {
			return true
		}
	}
	return false
}

// IsEmpty reports whether nothing is bound at all.
func (s SourceSet) IsEmpty() bool { return len(s.entries) == 0 }

// json encodes the set the way the module reads it, for renderer r:
// [{"data": id, "coord": [dx, dy], "sourceZoom": z}, …]. Entries are sorted
// so the same set is always the same bytes.
func (s SourceSet) json(r *Renderer) ([]byte, error) {
	type wireEntry struct {
		Data       uint32   `json:"data"`
		Coord      [2]int32 `json:"coord"`
		SourceZoom *int32   `json:"sourceZoom,omitempty"`
	}
	keys := make([]setKey, 0, len(s.entries))
	for k := range s.entries {
		keys = append(keys, k)
	}
	sort.Slice(keys, func(i, j int) bool {
		a, b := keys[i], keys[j]
		if a.source != b.source {
			return a.source < b.source
		}
		if a.at.DY != b.at.DY {
			return a.at.DY < b.at.DY
		}
		return a.at.DX < b.at.DX
	})
	wire := make([]wireEntry, 0, len(keys))
	for _, k := range keys {
		e := s.entries[k]
		if e.data.r != r {
			return nil, ErrForeignSource
		}
		id := e.data.id.Load()
		if id == 0 {
			return nil, fmt.Errorf("source %q at %+v: %w", k.source, k.at, ErrSourceReleased)
		}
		w := wireEntry{Data: id, Coord: [2]int32{k.at.DX, k.at.DY}}
		if e.hasSourceZoom {
			z := e.sourceZoom
			w.SourceZoom = &z
		}
		wire = append(wire, w)
	}
	return json.Marshal(wire)
}

// RenderTileWith renders one tile from the tile data in set and returns the
// encoded bytes, as [Renderer.RenderTile] does from what is bound. Nothing
// tile-scoped that BindSource bound is read; the fonts, sprites, brushes,
// images and glyphs bound into the renderer are.
//
// It changes nothing a later render depends on, so it needs no
// [Renderer.ClearSources] and works while [Renderer.RenderFrame] holds the
// renderer. It does not bring the glyph banks back under a budget set with
// [Renderer.SetGlyphBudget], as RenderTile does after each render; call
// [Renderer.TrimGlyphs] when you are done with a batch.
//
// Every SourceData in set must have been loaded by this renderer
// ([ErrForeignSource]) and not released ([ErrSourceReleased]).
func (r *Renderer) RenderTileWith(ctx context.Context, tile Tile, set SourceSet, opts Render) ([]byte, error) {
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
		return r.call1(ctx, "ezu_render_tile_with",
			uint64(r.handle),
			uint64(uint32(tile.Z)), uint64(uint32(tile.X)), uint64(uint32(tile.Y)),
			uint64(setPtr), uint64(len(setJSON)),
			uint64(optsPtr), uint64(len(optsJSON)),
			uint64(slots), uint64(slots+4))
	})
	return payload, err
}

// NeededCodepointsIn is [Renderer.NeededCodepoints] for the features in set
// rather than the ones bound to the renderer: what to bind into the glyphs
// sources before rendering from it.
func (r *Renderer) NeededCodepointsIn(ctx context.Context, set SourceSet) (map[string][]uint32, error) {
	return r.glyphUnitsIn(ctx, "ezu_needed_codepoints_in", set)
}

// NeededGlyphRangesIn is [Renderer.NeededGlyphRanges] for the features in
// set rather than the ones bound to the renderer.
func (r *Renderer) NeededGlyphRangesIn(ctx context.Context, set SourceSet) (map[string][]uint32, error) {
	return r.glyphUnitsIn(ctx, "ezu_needed_glyph_ranges_in", set)
}

func (r *Renderer) glyphUnitsIn(ctx context.Context, export string, set SourceSet) (map[string][]uint32, error) {
	setJSON, err := set.json(r)
	if err != nil {
		return nil, err
	}
	out := map[string][]uint32{}
	_, payload, err := r.withSlots(ctx, func(slots uint32) (int64, error) {
		setPtr, err := r.writeBytes(ctx, setJSON)
		if err != nil {
			return 0, err
		}
		defer r.free(ctx, setPtr, uint32(len(setJSON)))
		return r.call1(ctx, export, uint64(r.handle),
			uint64(setPtr), uint64(len(setJSON)), uint64(slots), uint64(slots+4))
	})
	if err != nil {
		return nil, err
	}
	if err := json.Unmarshal(payload, &out); err != nil {
		return nil, fmt.Errorf("ezu: reading %s: %w", export, err)
	}
	return out, nil
}

// TrimGlyphs brings every bound fontstack back under the budget
// [Renderer.SetGlyphBudget] set, and does nothing without one.
// [Renderer.RenderTile] does this after each render; after
// [Renderer.RenderTileWith], call it yourself once a batch of renders is
// done. Trimming can drop glyphs a frame bound for its next tile, so it
// fails with [ErrFrameInProgress] while [Renderer.RenderFrame] holds the
// renderer, which trims after each of its tiles itself.
func (r *Renderer) TrimGlyphs(ctx context.Context) error {
	if err := r.idle(); err != nil {
		return err
	}
	return r.trimGlyphs(ctx)
}

func (r *Renderer) trimGlyphs(ctx context.Context) error {
	_, err := r.call(ctx, func(ctx context.Context) (int64, error) {
		return r.call1(ctx, "ezu_trim_glyphs", uint64(r.handle))
	})
	return err
}
