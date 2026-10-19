package ezu

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"image"
	"math"
	"testing"
)

// The claim here is the one render_test.go makes for a tile, made for a
// print frame: the module computes and renders what `ezu frame` does. The
// numbers and the hash below came from the CLI.

// A 245 × 174 mm page at 1:1000 and 200 dpi over Tokyo, against
// stained-glass's 512 px tile size. The CLI logs, for
//
//	target/release/ezu frame --style crates/ezu/examples/styles/stained-glass.json \
//	  --center 139.70,35.68 --scale 1000 --dpi 200 --size-px 1929x1370 …
//
// "1929×1370 px at z=19, tile-size 489 (style: 512), scale 1:999.8, shift
// (+0.47, -0.27) px, 20 tiles (465594..465599, 206453..206457)".
func TestComputeFrameMatchesTheCLI(t *testing.T) {
	ctx, renderer := open(t, stainedGlassStyle)
	req := FrameRequest{
		CenterLon: 139.70, CenterLat: 35.68,
		Scale: 1000, DPI: 200,
		Width:  MillimetresToPixels(245, 200),
		Height: MillimetresToPixels(174, 200),
	}
	if req.Width != 1929 || req.Height != 1370 {
		t.Fatalf("245 × 174 mm at 200 dpi is %d × %d px, want 1929 × 1370", req.Width, req.Height)
	}
	f, err := renderer.ComputeFrame(ctx, req)
	if err != nil {
		t.Fatal(err)
	}
	if f.Zoom != 19 || f.TileSize != 489 {
		t.Errorf("z=%d, tile size %d; the CLI renders z=19 at 489", f.Zoom, f.TileSize)
	}
	if math.Abs(f.Scale-999.8) > 0.05 {
		t.Errorf("scale 1:%.2f, the CLI meets 1:999.8", f.Scale)
	}
	if math.Abs(f.ShiftX-0.47) > 0.005 || math.Abs(f.ShiftY+0.27) > 0.005 {
		t.Errorf("shift (%+.3f, %+.3f), the CLI moves the frame (+0.47, -0.27)", f.ShiftX, f.ShiftY)
	}
	if want := (TileRange{465594, 465599}); f.TilesX != want {
		t.Errorf("columns %v, want %v", f.TilesX, want)
	}
	if want := (TileRange{206453, 206457}); f.TilesY != want {
		t.Errorf("rows %v, want %v", f.TilesY, want)
	}
	if f.Width != 1929 || f.Height != 1370 || f.TileCount() != 20 || len(f.Tiles()) != 20 {
		t.Errorf("%d × %d px over %d tiles, want 1929 × 1370 over 20", f.Width, f.Height, f.TileCount())
	}
	// The first tile starts at or before the frame's corner, less than a
	// tile before it — which is what places every other tile.
	dx, dy := f.TileOffset(Tile{Z: f.Zoom, X: f.TilesX.Start, Y: f.TilesY.Start})
	if dx > 0 || dx <= -int64(f.TileSize) || dy > 0 || dy <= -int64(f.TileSize) {
		t.Errorf("the first tile lands at (%d, %d)", dx, dy)
	}

	// A fixed zoom derives the tile size from it: one zoom shallower needs
	// tiles twice the size for the same scale.
	at18, err := renderer.ComputeFrame(ctx, req, AtZoom(18))
	if err != nil {
		t.Fatal(err)
	}
	if at18.Zoom != 18 || math.Abs(float64(at18.TileSize)-2*float64(f.TileSize)) > 1 {
		t.Errorf("AtZoom(18) gave z=%d at %d px", at18.Zoom, at18.TileSize)
	}

	// The style tile size is what the default zoom aims at, so naming a
	// much smaller one moves the zoom deeper: 489 px at z19 is 122 at z21.
	small, err := renderer.ComputeFrame(ctx, FrameRequest{
		CenterLon: 139.70, CenterLat: 35.68, Scale: 1000, DPI: 200,
		Width: 1929, Height: 1370, StyleTileSize: 128,
	})
	if err != nil {
		t.Fatal(err)
	}
	if small.Zoom != 21 || small.TileSize != 122 {
		t.Errorf("aimed at 128 px the frame renders z=%d at %d px, want z=21 at 122", small.Zoom, small.TileSize)
	}

	if _, err := renderer.ComputeFrame(ctx, FrameRequest{
		CenterLon: 139.70, CenterLat: 35.68, Scale: 0, DPI: 200, Width: 10, Height: 10,
	}); !IsKind(err, KindInvalidStyle) {
		t.Errorf("a zero scale gave %v, want %s", err, KindInvalidStyle)
	}
}

func TestSetTileSizeRendersAtThatSize(t *testing.T) {
	g := goldens[0]
	ctx, renderer := open(t, g.style)
	if err := renderer.BindSource(ctx, "basemap", readFile(t, g.tileMVT)); err != nil {
		t.Fatal(err)
	}
	perRender, err := renderer.RenderTile(ctx, g.tile, Render{Format: FormatRGBA, TileSize: 93})
	if err != nil {
		t.Fatal(err)
	}

	if err := renderer.SetTileSize(ctx, 93); err != nil {
		t.Fatal(err)
	}
	if size, err := renderer.TileSize(ctx); err != nil || size != 512 {
		t.Errorf("TileSize is %d (%v) after SetTileSize; it reports the style's 512", size, err)
	}
	set, err := renderer.RenderTile(ctx, g.tile, Render{Format: FormatRGBA})
	if err != nil {
		t.Fatal(err)
	}
	if len(set) != 93*93*4 || !bytes.Equal(set, perRender) {
		t.Errorf("SetTileSize(93) rendered %d bytes, not the %d of a 93 px render", len(set), len(perRender))
	}

	// A per-render size still wins.
	if px, err := renderer.RenderTile(ctx, g.tile, Render{Format: FormatRGBA, TileSize: 64}); err != nil || len(px) != 64*64*4 {
		t.Errorf("a 64 px render over SetTileSize(93) gave %d bytes (%v)", len(px), err)
	}

	if err := renderer.ClearTileSize(ctx); err != nil {
		t.Fatal(err)
	}
	back, err := renderer.RenderTile(ctx, g.tile, Render{})
	if err != nil {
		t.Fatal(err)
	}
	if got := sha256hex(back); got != g.png {
		t.Errorf("after ClearTileSize the tile is %s, not the style's own %s", got, g.png)
	}

	if err := renderer.SetTileSize(ctx, 0); err == nil {
		t.Error("SetTileSize(0) was accepted")
	}

	// A new style starts at its own size.
	if err := renderer.SetTileSize(ctx, 93); err != nil {
		t.Fatal(err)
	}
	if _, err := renderer.SetStyle(ctx, readFile(t, g.style)); err != nil {
		t.Fatal(err)
	}
	if err := renderer.BindSource(ctx, "basemap", readFile(t, g.tileMVT)); err != nil {
		t.Fatal(err)
	}
	if px, err := renderer.RenderTile(ctx, g.tile, Render{Format: FormatRGBA}); err != nil || len(px) != 512*512*4 {
		t.Errorf("after SetStyle a tile is %d bytes (%v), want the style's 512 px", len(px), err)
	}
}

// A 500 × 400 px frame on the corner where four z14 tiles meet, so every
// tile it renders is cropped on two sides, at a tile size (391) that is
// nobody's default. Only their z13 parent is in testdata, so both sides
// overzoom it: the CLI falls back to the ancestor when the z14 fetch misses,
// and the bind below says FromZoom(13). The hash is of the pixels of the
// CLI's PNG, decoded:
//
//	mkdir -p <dir>/13/7277 && cp go/testdata/basemap-13-7277-3227.mvt <dir>/13/7277/3227.mvt
//	target/release/ezu frame --style crates/ezu/examples/styles/stained-glass.json \
//	  --mvt '<dir>/{z}/{x}/{y}.mvt' --center 139.812,35.6216 --scale 40000 \
//	  --dpi 200 --size-px 500x400 --zoom 14 --out frame.png
//
// Regenerate it the same way if a deliberate change to the renderer moves
// the output.
const cornerFramePixels = "05a14d1e9de68a304c0186b8dbe49722af5d83e3827dd3aefbad820a39b5e208"

func TestRenderFrameMatchesTheCLI(t *testing.T) {
	ctx, renderer := open(t, stainedGlassStyle)
	f, err := renderer.ComputeFrame(ctx, FrameRequest{
		CenterLon: 139.812, CenterLat: 35.6216,
		Scale: 40000, DPI: 200,
		Width: 500, Height: 400,
	}, AtZoom(14))
	if err != nil {
		t.Fatal(err)
	}
	if f.TileSize != 391 || f.TilesX != (TileRange{14554, 14556}) || f.TilesY != (TileRange{6454, 6456}) {
		t.Fatalf("the frame is %+v; the CLI renders 2 × 2 z14 tiles from 14554/6454 at 391 px", f)
	}

	parent := readFile(t, "testdata/basemap-13-7277-3227.mvt")
	var bound []Tile
	bind := func(ctx context.Context, tile Tile) error {
		bound = append(bound, tile)
		return renderer.BindSource(ctx, "basemap", parent, FromZoom(13))
	}
	img, err := renderer.RenderFrame(ctx, f, bind, Render{})
	if err != nil {
		t.Fatal(err)
	}
	if b := img.Bounds(); b != image.Rect(0, 0, 500, 400) {
		t.Fatalf("the frame image is %v, want 500 × 400", b)
	}
	if len(bound) != 4 || bound[0] != (Tile{14, 14554, 6454}) || bound[3] != (Tile{14, 14555, 6455}) {
		t.Errorf("bind saw %v; want the four tiles, row by row", bound)
	}
	sum := sha256.Sum256(img.Pix)
	if got := hex.EncodeToString(sum[:]); got != cornerFramePixels {
		t.Errorf("frame pixels %s, the CLI's are %s", got, cornerFramePixels)
	}

	// The renderer's own tile size is untouched, and so is its next tile.
	if _, err := renderer.RenderTile(ctx, Tile{Z: 14, X: 14554, Y: 6454}, Render{}); err != nil {
		t.Fatal(err)
	}
	if px, err := renderer.RenderTile(ctx, Tile{Z: 14, X: 14554, Y: 6454}, Render{Format: FormatRGBA}); err != nil || len(px) != 512*512*4 {
		t.Errorf("after RenderFrame a tile is %d bytes (%v), want the style's 512 px", len(px), err)
	}

	if _, err := renderer.RenderFrame(ctx, f, bind, Render{Format: FormatPNG}); err == nil {
		t.Error("RenderFrame accepted a PNG format")
	}
	if _, err := renderer.RenderFrame(ctx, f, bind, Render{TileSize: 256}); err == nil {
		t.Error("RenderFrame accepted a tile size other than the frame's")
	}
}

func TestBlitTileCropsAtEveryEdge(t *testing.T) {
	// A 4 × 4 tile whose bytes count up, placed so that only its centre
	// 2 × 2 falls in a 2 × 2 image.
	tile := make([]byte, 4*4*4)
	for i := range tile {
		tile[i] = byte(i)
	}
	dst := image.NewNRGBA(image.Rect(0, 0, 2, 2))
	blitTile(dst, tile, 4, -1, -1)
	for y := 0; y < 2; y++ {
		for x := 0; x < 2; x++ {
			src := ((y+1)*4 + (x + 1)) * 4
			if got := dst.Pix[dst.PixOffset(x, y)]; got != tile[src] {
				t.Errorf("(%d, %d) has %d, want %d", x, y, got, tile[src])
			}
		}
	}
	// Wholly outside: nothing written, nothing panics.
	blitTile(dst, make([]byte, 4*4*4), 4, 2, 0)
	blitTile(dst, make([]byte, 4*4*4), 4, -4, 0)
	if dst.Pix[0] != tile[(4+1)*4] {
		t.Error("a tile outside the image wrote into it")
	}
}
