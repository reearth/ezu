package ezu

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"image"
	"image/png"
	"testing"
	"time"
)

// The claim this file exists to hold: **the module renders the same bytes
// the native renderer does.** Not similar pixels — the same file.
//
// Nothing in ezu's render path depends on the host, so there is no reason
// for a tile rendered through wazero to differ from one rendered by the
// CLI, and a difference would mean something host-dependent has crept in.
// The hashes below were produced by the CLI, which is the reference:
//
//	target/release/ezu tile \
//	  --style crates/ezu/examples/styles/stained-glass.json \
//	  --mvt '<dir>/{z}/{x}/{y}.mvt' --tile 14/14554/6454 --out out.png
//
// with go/testdata/basemap-<z>-<x>-<y>.mvt laid out under <dir>. Regenerate
// them the same way if a deliberate change to the renderer moves the output
// — and if one moves without a deliberate change, that is the finding.
//
// PNG is identical between the two, on both tiles. **WebP is not, and the
// difference is in the container rather than the picture.** For
// stained-glass the CLI writes 5165cf90… and the module writes 63677330…,
// both 185,338 bytes, and decoding the two files gives byte-identical RGBA:
// 0 of 1,048,576 samples differ. So the encoder reached a different but
// equally sized encoding of the same pixels.
//
// It is not the SIMD build — a module compiled without `+simd128` produces
// the same bytes as the one with it, for both formats. The likely cause is
// that the lossless encoder's cost estimates go through floating-point
// transcendentals, which Rust serves from the platform's libm natively and
// from its own on wasm; a last-bit difference there moves a tie and picks
// another encoding of the same size. PNG is unaffected because deflate does
// no floating-point arithmetic at all. risograph's WebP happens to agree
// across both, which is what a data-dependent tie-break looks like.
//
// So the WebP rows below are the **module's** output, pinned to catch drift
// in it, and not a cross-implementation claim. If you regenerate them,
// regenerate them from the module.
var goldens = []struct {
	name    string
	style   string
	tileMVT string
	tile    Tile
	png     string
	webp    string
}{
	{
		name:    "stained-glass",
		style:   stainedGlassStyle,
		tileMVT: "testdata/basemap-14-14554-6454.mvt",
		tile:    Tile{Z: 14, X: 14554, Y: 6454},
		png:  "daeb73b96d495681a2d95c3043a5ce65a27d3e3ce857e95c10255a5c5861d69a",
		webp: "63677330cdd1e31f9fcaa12aaffa35ee691362c3b34e3d9ac9b9b665b9fc617f",
	},
	{
		name:    "risograph",
		style:   risographStyle,
		tileMVT: "testdata/basemap-13-7277-3227.mvt",
		tile:    Tile{Z: 13, X: 7277, Y: 3227},
		png:  "1f30f5442904edbdc70cd9f736cb01df85058bcfd31dd2f3b0259961c284faf2",
		webp: "35f31eff6f470ad0be48a52af9f2fe0ca70331ea03ce331e12aff86ddca3b78f",
	},
}

func sha256hex(b []byte) string {
	sum := sha256.Sum256(b)
	return hex.EncodeToString(sum[:])
}

func TestRenderMatchesTheNativeRenderer(t *testing.T) {
	for _, g := range goldens {
		t.Run(g.name, func(t *testing.T) {
			ctx, renderer := open(t, g.style)
			if err := renderer.BindSource(ctx, "basemap", readFile(t, g.tileMVT)); err != nil {
				t.Fatal(err)
			}

			gotPNG, err := renderer.RenderTile(ctx, g.tile, Render{})
			if err != nil {
				t.Fatal(err)
			}
			if got := sha256hex(gotPNG); got != g.png {
				t.Errorf("PNG sha256 %s (%d bytes), want %s", got, len(gotPNG), g.png)
			}

			gotWebP, err := renderer.RenderTile(ctx, g.tile, Render{Format: FormatWebP})
			if err != nil {
				t.Fatal(err)
			}
			// The module's own WebP, not the CLI's — see the note above.
			if got := sha256hex(gotWebP); got != g.webp {
				t.Errorf("WebP sha256 %s (%d bytes), want %s", got, len(gotWebP), g.webp)
			}

			// RGBA has no container and so no CLI counterpart to hash
			// against. The check that means something is that it is the
			// same picture as the PNG, pixel for pixel — which is also what
			// says the crop and the row order are right.
			gotRGBA, err := renderer.RenderTile(ctx, g.tile, Render{Format: FormatRGBA})
			if err != nil {
				t.Fatal(err)
			}
			size, err := renderer.TileSize(ctx)
			if err != nil {
				t.Fatal(err)
			}
			if want := int(size) * int(size) * 4; len(gotRGBA) != want {
				t.Fatalf("RGBA is %d bytes, want %d for a %dx%d tile", len(gotRGBA), want, size, size)
			}
			assertSamePicture(t, gotPNG, gotRGBA, int(size))
		})
	}
}

// assertSamePicture decodes the PNG and compares it with the raw RGBA.
//
// The PNG carries un-premultiplied straight alpha and so does the RGBA
// output, so the two are comparable directly — but Go's image/png hands
// back an NRGBA only for a truecolour-with-alpha file, which is what ezu
// writes.
func assertSamePicture(t *testing.T, pngBytes, rgba []byte, size int) {
	t.Helper()
	img, err := png.Decode(bytes.NewReader(pngBytes))
	if err != nil {
		t.Fatalf("the PNG did not decode: %v", err)
	}
	if b := img.Bounds(); b.Dx() != size || b.Dy() != size {
		t.Fatalf("the PNG is %dx%d, want %dx%d", b.Dx(), b.Dy(), size, size)
	}
	nrgba, ok := img.(*image.NRGBA)
	if !ok {
		t.Skipf("the PNG decoded as %T rather than NRGBA; nothing to compare directly", img)
	}
	for y := 0; y < size; y++ {
		for x := 0; x < size; x++ {
			i := (y*size + x) * 4
			j := nrgba.PixOffset(x, y)
			if !bytes.Equal(rgba[i:i+4], nrgba.Pix[j:j+4]) {
				t.Fatalf("pixel (%d, %d): rgba %v, png %v", x, y, rgba[i:i+4], nrgba.Pix[j:j+4])
			}
		}
	}
}

// Rendering the same tile twice must give the same bytes: the renderer
// seeds its randomness from the tile id, which is what lets a tile server
// cache and a browser preview agree.
func TestRenderIsDeterministic(t *testing.T) {
	g := goldens[0]
	ctx, renderer := open(t, g.style)
	if err := renderer.BindSource(ctx, "basemap", readFile(t, g.tileMVT)); err != nil {
		t.Fatal(err)
	}
	first, err := renderer.RenderTile(ctx, g.tile, Render{})
	if err != nil {
		t.Fatal(err)
	}
	second, err := renderer.RenderTile(ctx, g.tile, Render{})
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(first, second) {
		t.Error("two renders of one tile differ")
	}
}

// The render options that change the canvas or the encoder. These do not
// have goldens — what they assert is that the option reached the renderer
// at all, which is the part a shell gets wrong.
func TestRenderOptionsReachTheRenderer(t *testing.T) {
	g := goldens[0]
	ctx, renderer := open(t, g.style)
	if err := renderer.BindSource(ctx, "basemap", readFile(t, g.tileMVT)); err != nil {
		t.Fatal(err)
	}

	small, err := renderer.RenderTile(ctx, g.tile, Render{Format: FormatRGBA, TileSize: 64, Pad: 4})
	if err != nil {
		t.Fatal(err)
	}
	if want := 64 * 64 * 4; len(small) != want {
		t.Errorf("a tileSize override gave %d bytes, want %d", len(small), want)
	}

	fast, err := renderer.RenderTile(ctx, g.tile, Render{PNGCompression: CompressionFast})
	if err != nil {
		t.Fatal(err)
	}
	best, err := renderer.RenderTile(ctx, g.tile, Render{PNGCompression: CompressionBest})
	if err != nil {
		t.Fatal(err)
	}
	if len(fast) <= len(best) {
		t.Errorf("fast compression produced %d bytes and best %d; the option did not take", len(fast), len(best))
	}
}

// What a tile costs, reported rather than asserted: the number depends on
// the machine and the style, and a threshold here would fail on somebody
// else's laptop rather than on a regression.
func TestRenderTiming(t *testing.T) {
	if testing.Short() {
		t.Skip("timing")
	}
	g := goldens[0]
	ctx, renderer := open(t, g.style)
	if err := renderer.BindSource(ctx, "basemap", readFile(t, g.tileMVT)); err != nil {
		t.Fatal(err)
	}

	start := time.Now()
	out, err := renderer.RenderTile(ctx, g.tile, Render{})
	if err != nil {
		t.Fatal(err)
	}
	first := time.Since(start)

	best := time.Duration(1 << 62)
	const runs = 10
	for i := 0; i < runs; i++ {
		start = time.Now()
		if _, err := renderer.RenderTile(ctx, g.tile, Render{}); err != nil {
			t.Fatal(err)
		}
		if d := time.Since(start); d < best {
			best = d
		}
	}
	usage, err := renderer.MemoryUsage(ctx)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("%s: first render %v, best of %d more %v, %d PNG bytes, heap %d bytes",
		g.name, first, runs, best, len(out), usage.HeapBytes)
}

func BenchmarkRenderTile(b *testing.B) {
	ctx := context.Background()
	g := goldens[0]
	rt, err := NewRuntime(ctx)
	if err != nil {
		b.Fatal(err)
	}
	defer rt.Close(ctx)
	style, err := readFileNoT(g.style)
	if err != nil {
		b.Fatal(err)
	}
	renderer, err := rt.NewRenderer(ctx, style)
	if err != nil {
		b.Fatal(err)
	}
	defer renderer.Close(ctx)
	tile, err := readFileNoT(g.tileMVT)
	if err != nil {
		b.Fatal(err)
	}
	if err := renderer.BindSource(ctx, "basemap", tile); err != nil {
		b.Fatal(err)
	}
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		if _, err := renderer.RenderTile(ctx, g.tile, Render{}); err != nil {
			b.Fatal(err)
		}
	}
}
