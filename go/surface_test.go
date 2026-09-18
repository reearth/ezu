package ezu

import (
	"encoding/json"
	"strings"
	"testing"
)

// The query side of the ABI. Each of these crosses the boundary as JSON,
// and what goes wrong with JSON over a flat ABI is not the content but the
// plumbing — an out-slot read before it was written, a buffer freed twice,
// a field name that differs from the JavaScript shell's. So these check
// that the answers arrive and are shaped right, rather than restating what
// the renderer computed.

func TestStyleQueries(t *testing.T) {
	ctx, renderer := open(t, atlasStyle)

	size, err := renderer.TileSize(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if size == 0 {
		t.Error("tile size 0")
	}

	attribution, err := renderer.Attribution(ctx)
	if err != nil {
		t.Fatal(err)
	}
	// atlas.json declares attribution on its font source, so this style is
	// the one that shows the joining actually happens.
	if !strings.Contains(attribution, "Open Sans") {
		t.Errorf("attribution %q does not mention the font source's", attribution)
	}

	schema, err := renderer.ParamsSchema(ctx)
	if err != nil {
		t.Fatal(err)
	}
	var doc map[string]any
	if err := json.Unmarshal(schema, &doc); err != nil {
		t.Fatalf("the params schema is not a JSON object: %v", err)
	}
	if doc["type"] != "object" {
		t.Errorf("the params schema has type %v, want object", doc["type"])
	}

	// A style with no legend answers nil rather than an error or an empty
	// document, which is the distinction a host renders on.
	legend, err := renderer.Legend(ctx, AllZooms)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("tile size %d, attribution %q, legend present: %v", size, attribution, legend != nil)
}

// Sources is where a bind loop starts, so what it has to carry is
// everything a loop branches on: the name to bind under, the kind, whether
// the binding survives a ClearSources, and the address to fetch from.
// testdata/labels.json has both a tile pyramid and a glyph endpoint, which
// is the one whose URL is not simply what the style wrote.
func TestSourcesDescribeTheBindLoop(t *testing.T) {
	ctx, renderer := open(t, "testdata/labels.json")

	sources, err := renderer.Sources(ctx)
	if err != nil {
		t.Fatal(err)
	}
	want := []Source{
		{Name: "basemap", Type: "mvt", TileScoped: true,
			URL: "https://papers.reearth.land/protomaps/tilejson.json"},
		// {fontstack} comes back substituted and percent-encoded the way
		// MapLibre spells it; {range} stays, since it is fetched per block.
		{Name: "noto", Type: "glyphs", TileScoped: false,
			URL: "https://example.invalid/fonts/Noto%20Sans%20Regular/{range}.pbf"},
	}
	if len(sources) != len(want) {
		t.Fatalf("the style declares %d sources, got %v", len(want), sources)
	}
	for i, w := range want {
		if sources[i] != w {
			t.Errorf("source %d is %+v, want %+v", i, sources[i], w)
		}
	}

	// And the loop it describes runs: the one tile-scoped source binds
	// under the name it reported.
	if err := renderer.BindSource(ctx, sources[0].Name,
		readFile(t, "testdata/basemap-14-14554-6454.mvt")); err != nil {
		t.Fatal(err)
	}
}

func TestBoundSourcesFollowTheBindings(t *testing.T) {
	ctx, renderer := open(t, stainedGlassStyle)

	bound, err := renderer.BoundSources(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(bound) != 0 {
		t.Errorf("a fresh renderer reports %v bound", bound)
	}

	if err := renderer.BindSource(ctx, "basemap", readFile(t, "testdata/basemap-14-14554-6454.mvt")); err != nil {
		t.Fatal(err)
	}
	if bound, err = renderer.BoundSources(ctx); err != nil {
		t.Fatal(err)
	} else if len(bound) != 1 || bound[0] != "basemap" {
		t.Errorf("bound sources %v, want [basemap]", bound)
	}

	if err := renderer.ClearSources(ctx); err != nil {
		t.Fatal(err)
	}
	if bound, err = renderer.BoundSources(ctx); err != nil {
		t.Fatal(err)
	} else if len(bound) != 0 {
		t.Errorf("bound sources %v after ClearSources, want none", bound)
	}
}

// The fetch-planning calls, which are what a host walks before it fetches
// anything. A DEM source asks for its whole 3×3 by default, so hillshade is
// the style that shows a non-empty answer.
func TestNeighborPlanning(t *testing.T) {
	ctx, renderer := open(t, hillshadeStyle)

	offsets, err := renderer.RequestedNeighborOffsets(ctx, "terrain")
	if err != nil {
		t.Fatal(err)
	}
	if len(offsets) != 8 {
		t.Errorf("a dem source with neighbor-fetch on asked for %d offsets, want the 8 around it: %v",
			len(offsets), offsets)
	}
	for _, o := range offsets {
		if o.DX == 0 && o.DY == 0 {
			t.Error("the centre tile is in the neighbour list; it should never be")
		}
	}

	// A source the style does not declare is UnknownSource, the same name
	// the JavaScript shell throws.
	if _, err := renderer.RequestedNeighborOffsets(ctx, "nope"); !IsKind(err, KindUnknownSrc) {
		t.Errorf("an undeclared source gave %v, want UnknownSource", err)
	}
}

// SourceTile is where a source's max-zoom ceiling lives, so that a host
// does not keep a second copy of it.
func TestSourceTileAnswersTheCeiling(t *testing.T) {
	ctx, renderer := open(t, hillshadeStyle)

	// Below any ceiling, the answer is the tile itself.
	got, err := renderer.SourceTile(ctx, "terrain", Tile{Z: 10, X: 909, Y: 402})
	if err != nil {
		t.Fatal(err)
	}
	if got != (Tile{Z: 10, X: 909, Y: 402}) {
		t.Errorf("a shallow tile answered %+v, want itself", got)
	}

	// Deep enough to be past whatever ceiling the style declares: the
	// answer must be an ancestor of the request, whatever the ceiling is.
	deep := Tile{Z: 22, X: 3728270, Y: 1649855}
	got, err = renderer.SourceTile(ctx, "terrain", deep)
	if err != nil {
		t.Fatal(err)
	}
	if got.Z > deep.Z {
		t.Fatalf("answered zoom %d for a request at %d", got.Z, deep.Z)
	}
	if shift := deep.Z - got.Z; got.X != deep.X>>shift || got.Y != deep.Y>>shift {
		t.Errorf("answered %+v, which is not the ancestor of %+v at zoom %d", got, deep, got.Z)
	}

	// X wraps around the antimeridian; Y does not.
	west, err := renderer.SourceTile(ctx, "terrain", Tile{Z: 2, X: 0, Y: 1}.Add(Offset{DX: -1}))
	if err != nil {
		t.Fatal(err)
	}
	if west.X != 3 {
		t.Errorf("the western neighbour of 2/0/1 answered x=%d, want 3 (the far side of the world)", west.X)
	}
}

// The glyph prepass: what a host has to fetch before it can draw a label.
//
// This host cannot fetch glyphs lazily, so everything a tile's text touches
// must be bound before the render, and scraping the MVT for it is the
// host's alternative. testdata/labels.json is the style that asks the
// question — a glyphs source and one text layer over the basemap's `places`
// — since the shipped examples use outline `font` sources, which have no
// ranges to bind.
func TestGlyphPrepass(t *testing.T) {
	ctx, renderer := open(t, "testdata/labels.json")
	if err := renderer.BindSource(ctx, "basemap", readFile(t, "testdata/basemap-14-14554-6454.mvt")); err != nil {
		t.Fatal(err)
	}
	codepoints, err := renderer.NeededCodepoints(ctx)
	if err != nil {
		t.Fatal(err)
	}
	ranges, err := renderer.NeededGlyphRanges(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(codepoints) == 0 {
		t.Fatal("no glyphs source reported anything; the prepass saw no text layer")
	}
	if len(codepoints["noto"]) == 0 {
		t.Error("the `noto` glyphs source needs no codepoints, though the tile has named places")
	}
	if len(codepoints) != len(ranges) {
		t.Errorf("%d glyph sources by codepoint and %d by range; they see the same sources",
			len(codepoints), len(ranges))
	}
	for source, cps := range codepoints {
		// Every range must be the block containing a codepoint that was
		// asked for, since one is derived from the other.
		blocks := map[uint32]bool{}
		for _, cp := range cps {
			blocks[cp&^0xFF] = true
		}
		for _, r := range ranges[source] {
			if !blocks[r] {
				t.Errorf("source %s: range %d covers no needed codepoint", source, r)
			}
		}
	}
	t.Logf("glyph sources: %d", len(codepoints))
}

func TestMemoryUsageAndGlyphBudget(t *testing.T) {
	ctx, renderer := open(t, stainedGlassStyle)

	usage, err := renderer.MemoryUsage(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if usage.HeapBytes == 0 {
		t.Error("heap bytes 0")
	}
	if usage.GlyphBudget != nil {
		t.Errorf("a fresh renderer reports a glyph budget of %d; none was set", *usage.GlyphBudget)
	}

	budget := uint64(1 << 20)
	if err := renderer.SetGlyphBudget(ctx, budget); err != nil {
		t.Fatal(err)
	}
	if usage, err = renderer.MemoryUsage(ctx); err != nil {
		t.Fatal(err)
	}
	if usage.GlyphBudget == nil || *usage.GlyphBudget != budget {
		t.Errorf("glyph budget reads back as %v, want %d", usage.GlyphBudget, budget)
	}

	// A budget of zero is a real budget — keep nothing — and must not be
	// confused with having none.
	if err := renderer.SetGlyphBudget(ctx, 0); err != nil {
		t.Fatal(err)
	}
	if usage, err = renderer.MemoryUsage(ctx); err != nil {
		t.Fatal(err)
	}
	if usage.GlyphBudget == nil || *usage.GlyphBudget != 0 {
		t.Errorf("a budget of 0 reads back as %v, want 0", usage.GlyphBudget)
	}

	// Clearing is the only way back to uncapped.
	if err := renderer.ClearGlyphBudget(ctx); err != nil {
		t.Fatal(err)
	}
	if usage, err = renderer.MemoryUsage(ctx); err != nil {
		t.Fatal(err)
	}
	if usage.GlyphBudget != nil {
		t.Errorf("a cleared budget reads back as %d, want none", *usage.GlyphBudget)
	}
	if usage.CacheBudget == 0 {
		t.Error("the render cache reports no eviction budget, so it would grow without bound")
	}
}

// Replacing the style keeps the instance and the persistent banks but drops
// the bindings and the cache.
func TestSetStyle(t *testing.T) {
	ctx, renderer := open(t, stainedGlassStyle)
	if err := renderer.BindSource(ctx, "basemap", readFile(t, "testdata/basemap-14-14554-6454.mvt")); err != nil {
		t.Fatal(err)
	}

	n, err := renderer.SetStyle(ctx, readFile(t, risographStyle))
	if err != nil {
		t.Fatal(err)
	}
	if n == 0 {
		t.Error("the replacement style built 0 nodes")
	}
	bound, err := renderer.BoundSources(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(bound) != 0 {
		t.Errorf("bindings %v survived a style replacement", bound)
	}

	// And the new style renders through the same handle.
	if err := renderer.BindSource(ctx, "basemap", readFile(t, "testdata/basemap-13-7277-3227.mvt")); err != nil {
		t.Fatal(err)
	}
	out, err := renderer.RenderTile(ctx, Tile{Z: 13, X: 7277, Y: 3227}, Render{})
	if err != nil {
		t.Fatal(err)
	}
	if got := sha256hex(out); got != goldens[1].png {
		t.Errorf("after SetStyle the tile hashes %s, want the risograph golden %s", got, goldens[1].png)
	}
}

// Render-time params are validated by the same parser the CLI's --param
// uses, so a value outside a declared range is refused here rather than
// rendering something quietly wrong.
func TestRenderParams(t *testing.T) {
	ctx, renderer := open(t, stainedGlassStyle)
	if err := renderer.BindSource(ctx, "basemap", readFile(t, "testdata/basemap-14-14554-6454.mvt")); err != nil {
		t.Fatal(err)
	}
	schema, err := renderer.ParamsSchema(ctx)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Properties map[string]json.RawMessage `json:"properties"`
	}
	if err := json.Unmarshal(schema, &doc); err != nil {
		t.Fatal(err)
	}
	if len(doc.Properties) == 0 {
		t.Skip("this style declares no params")
	}

	// A name the style does not declare is refused, which is what says the
	// params reached the validator at all rather than being dropped.
	_, err = renderer.RenderTile(ctx, Tile{Z: 14, X: 14554, Y: 6454}, Render{
		Params: map[string]any{"definitely-not-a-param": 1},
	})
	if !IsKind(err, KindInvalidStyle) {
		t.Errorf("an undeclared param gave %v, want InvalidStyle", err)
	}
}
