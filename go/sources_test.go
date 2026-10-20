package ezu

import (
	"bytes"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

// Rendering from a SourceSet is the bound table as a value: the same bytes
// at the same places draw the same tile, whichever way they reach the
// render.

// neighbours is every offset of the 3×3 neighbourhood but the centre.
func neighbours() []Offset {
	var out []Offset
	for dy := int32(-1); dy <= 1; dy++ {
		for dx := int32(-1); dx <= 1; dx++ {
			if dx != 0 || dy != 0 {
				out = append(out, Offset{DX: dx, DY: dy})
			}
		}
	}
	return out
}

// Water stitched across tile borders and blurred, so what sits at the
// neighbours reaches the pixels.
const stitchedStyle = `{
  "name": "stitched",
  "tile-size": 64,
  "sources": { "basemap": { "type": "mvt", "url": "https://example.invalid/{z}/{x}/{y}.mvt" } },
  "nodes": {
    "f": { "op": "features", "source": "basemap", "layer": "water", "stitch": "merge" },
    "fill": { "op": "fill-solid", "features": "@f", "fill": "#204080" },
    "out": { "op": "blur", "input": "@fill", "sigma": 3.0 }
  },
  "output": "out"
}`

// A z14 tile with its own bytes at the centre and its z13 parent,
// overzoomed, at each of the eight neighbours: the render from a set is
// byte for byte the render from the same bytes bound.
func TestRenderTileWithMatchesBindSource(t *testing.T) {
	stylePath := filepath.Join(t.TempDir(), "stitched.json")
	if err := os.WriteFile(stylePath, []byte(stitchedStyle), 0o644); err != nil {
		t.Fatal(err)
	}
	ctx, renderer := open(t, stylePath)
	tile := Tile{Z: 14, X: 14554, Y: 6454}
	centre := readFile(t, "testdata/basemap-14-14554-6454.mvt")
	parent := readFile(t, "testdata/basemap-13-7277-3227.mvt")

	if err := renderer.BindSource(ctx, "basemap", centre); err != nil {
		t.Fatal(err)
	}
	for _, o := range neighbours() {
		if err := renderer.BindSource(ctx, "basemap", parent, AtOffset(o), FromZoom(13)); err != nil {
			t.Fatal(err)
		}
	}
	bound, err := renderer.RenderTile(ctx, tile, Render{})
	if err != nil {
		t.Fatal(err)
	}
	if err := renderer.ClearSources(ctx); err != nil {
		t.Fatal(err)
	}

	centreData, err := renderer.LoadSource(ctx, "basemap", centre)
	if err != nil {
		t.Fatal(err)
	}
	parentData, err := renderer.LoadSource(ctx, "basemap", parent)
	if err != nil {
		t.Fatal(err)
	}
	if centreData.Source() != "basemap" {
		t.Errorf("the data is for %q", centreData.Source())
	}
	var set SourceSet
	if !set.IsEmpty() || set.Contains("basemap") {
		t.Error("the zero set is not empty")
	}
	if err := set.Bind(centreData); err != nil {
		t.Fatal(err)
	}
	for _, o := range neighbours() {
		if err := set.Bind(parentData, AtOffset(o), FromZoom(13)); err != nil {
			t.Fatal(err)
		}
	}
	if set.IsEmpty() || !set.Contains("basemap") || set.Contains("noto") {
		t.Error("the set does not say what it holds")
	}
	fromSet, err := renderer.RenderTileWith(ctx, tile, set, Render{})
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(fromSet, bound) {
		t.Errorf("the render from a set is %s, the bound render %s", sha256hex(fromSet), sha256hex(bound))
	}
	var centreOnly SourceSet
	if err := centreOnly.Bind(centreData); err != nil {
		t.Fatal(err)
	}
	if alone, err := renderer.RenderTileWith(ctx, tile, centreOnly, Render{}); err != nil || bytes.Equal(alone, fromSet) {
		t.Errorf("the neighbours changed nothing (%v), so this compares less than it claims", err)
	}
	if b, err := renderer.BoundSources(ctx); err != nil || len(b) != 0 {
		t.Errorf("rendering from a set bound %v (%v)", b, err)
	}
}

// One parent serves two children: loaded once, it sits in both tiles' sets
// and each draws what binding it for that tile alone draws.
func TestSourceDataServesSeveralTiles(t *testing.T) {
	ctx, renderer := open(t, stainedGlassStyle)
	parent := readFile(t, "testdata/basemap-13-7277-3227.mvt")
	data, err := renderer.LoadSource(ctx, "basemap", parent)
	if err != nil {
		t.Fatal(err)
	}
	defer data.Release(ctx)

	for _, tile := range []Tile{{Z: 14, X: 14554, Y: 6454}, {Z: 14, X: 14555, Y: 6454}} {
		var set SourceSet
		if err := set.Bind(data, FromZoom(13)); err != nil {
			t.Fatal(err)
		}
		fromSet, err := renderer.RenderTileWith(ctx, tile, set, Render{})
		if err != nil {
			t.Fatal(err)
		}

		if err := renderer.BindSource(ctx, "basemap", parent, FromZoom(13)); err != nil {
			t.Fatal(err)
		}
		bound, err := renderer.RenderTile(ctx, tile, Render{})
		if err != nil {
			t.Fatal(err)
		}
		if err := renderer.ClearSources(ctx); err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(fromSet, bound) {
			t.Errorf("%v: the render from the shared data differs from binding it", tile)
		}
	}
}

// Released data, and data from another renderer, are refused before they
// reach the module; so is what LoadSource has no use for.
func TestSourceDataBelongsToItsRenderer(t *testing.T) {
	ctx, renderer := open(t, "testdata/labels.json")
	tile := Tile{Z: 14, X: 14554, Y: 6454}
	mvt := readFile(t, "testdata/basemap-14-14554-6454.mvt")

	data, err := renderer.LoadSource(ctx, "basemap", mvt)
	if err != nil {
		t.Fatal(err)
	}
	var set SourceSet
	if err := set.Bind(data); err != nil {
		t.Fatal(err)
	}

	// The set's codepoints are the bound table's for the same bytes.
	fromSet, err := renderer.NeededCodepointsIn(ctx, set)
	if err != nil {
		t.Fatal(err)
	}
	rangesFromSet, err := renderer.NeededGlyphRangesIn(ctx, set)
	if err != nil {
		t.Fatal(err)
	}
	if err := renderer.BindSource(ctx, "basemap", mvt); err != nil {
		t.Fatal(err)
	}
	bound, _ := renderer.NeededCodepoints(ctx)
	boundRanges, _ := renderer.NeededGlyphRanges(ctx)
	if len(fromSet["noto"]) == 0 || !reflect.DeepEqual(fromSet, bound) || !reflect.DeepEqual(rangesFromSet, boundRanges) {
		t.Errorf("a set needs %d codepoints in %v, the bound tile %d in %v",
			len(fromSet["noto"]), rangesFromSet, len(bound["noto"]), boundRanges)
	}
	if empty, err := renderer.NeededCodepointsIn(ctx, SourceSet{}); err != nil || len(empty["noto"]) != 0 {
		t.Errorf("an empty set needs %v (%v)", empty, err)
	}

	// Another renderer, another wasm instance: the data is not there.
	rt, err := NewRuntime(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer rt.Close(ctx)
	other, err := rt.NewRenderer(ctx, readFile(t, "testdata/labels.json"))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := other.RenderTileWith(ctx, tile, set, Render{}); !errors.Is(err, ErrForeignSource) {
		t.Errorf("another renderer's data gave %v, want ErrForeignSource", err)
	}

	if err := data.Release(ctx); err != nil {
		t.Fatal(err)
	}
	if err := data.Release(ctx); err != nil {
		t.Errorf("a second Release gave %v", err)
	}
	if _, err := renderer.RenderTileWith(ctx, tile, set, Render{}); !errors.Is(err, ErrSourceReleased) {
		t.Errorf("a set holding released data gave %v, want ErrSourceReleased", err)
	}
	if _, err := renderer.NeededCodepointsIn(ctx, set); !errors.Is(err, ErrSourceReleased) {
		t.Errorf("asking about released data gave %v, want ErrSourceReleased", err)
	}
	var again SourceSet
	if err := again.Bind(data); !errors.Is(err, ErrSourceReleased) {
		t.Errorf("binding released data gave %v, want ErrSourceReleased", err)
	}
	if err := again.Bind(nil); !errors.Is(err, ErrSourceReleased) {
		t.Errorf("binding nil gave %v, want ErrSourceReleased", err)
	}

	if _, err := renderer.LoadSource(ctx, "noto", []byte{}); !IsKind(err, KindUnknownSrc) {
		t.Errorf("loading a glyphs source gave %v, want %s", err, KindUnknownSrc)
	}
	if _, err := renderer.LoadSource(ctx, "nowhere", mvt); !IsKind(err, KindUnknownSrc) {
		t.Errorf("loading an undeclared source gave %v, want %s", err, KindUnknownSrc)
	}
	if _, err := renderer.LoadSource(ctx, "basemap", []byte("not a tile")); !IsKind(err, "MvtDecode") {
		t.Errorf("loading a broken tile gave %v, want MvtDecode", err)
	}

	// Nothing above left the renderer unusable.
	if err := renderer.TrimGlyphs(ctx); err != nil {
		t.Fatal(err)
	}
	if _, err := renderer.RenderTileWith(ctx, tile, SourceSet{}, Render{}); err != nil {
		t.Errorf("rendering from an empty set gave %v", err)
	}
}
