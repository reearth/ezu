package ezu

import (
	"context"
	"os"
	"testing"
)

// nativeOpCount is what `cargo test -p ezu-cabi` reports for the same
// registry. The module must agree: a lower number means some of the
// `inventory::submit!` constructors never ran, and every style would fail
// with "unknown op".
const nativeOpCount = 85

const (
	stainedGlassStyle = "../crates/ezu/examples/styles/stained-glass.json"
	risographStyle    = "../crates/ezu/examples/styles/risograph.json"
	atlasStyle        = "../crates/ezu/examples/styles/atlas.json"
	hillshadeStyle    = "../crates/ezu/examples/styles/hillshade.json"
)

func readFile(t *testing.T, path string) []byte {
	t.Helper()
	b, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("reading %s: %v", path, err)
	}
	return b
}

// readFileNoT is readFile for a benchmark, which has no *testing.T.
func readFileNoT(path string) ([]byte, error) { return os.ReadFile(path) }

// open builds a runtime and one renderer over a style file, both closed
// when the test ends.
func open(t *testing.T, stylePath string, opts ...Option) (context.Context, *Renderer) {
	t.Helper()
	ctx := context.Background()
	rt, err := NewRuntime(ctx, opts...)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { rt.Close(ctx) })
	renderer, err := rt.NewRenderer(ctx, readFile(t, stylePath))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { renderer.Close(ctx) })
	return ctx, renderer
}

// The module is pinned to the reactor shape by crates/ezu-cabi/build.rs, so
// that no exported call drags __wasm_call_dtors behind it and tears down
// state this ABI keeps between calls. Both entry points into the
// constructors must be there: _initialize for a host that runs it on
// instantiation, and __wasm_call_ctors for one that does not.
func TestModuleIsReactorShaped(t *testing.T) {
	_, renderer := open(t, stainedGlassStyle)
	if !renderer.HasExport("_initialize") {
		t.Error("no _initialize: the module fell back to command-style linkage")
	}
	if !renderer.HasExport("__wasm_call_ctors") {
		t.Error("no __wasm_call_ctors: the host has no way to run the constructors")
	}
}

// What the reactor shape costs, and why the host's belt and braces are not
// decoration: an instance with neither entry point called has an empty
// registry. This is the failure the build script's comment names, held here
// so that it stays a deliberate property rather than a surprise.
func TestRegistryIsEmptyWithoutConstructors(t *testing.T) {
	ctx := context.Background()
	rt, err := NewRuntime(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer rt.Close(ctx)

	renderer, err := rt.instantiate(ctx, "no-ctors", startNoNets)
	if err != nil {
		t.Fatal(err)
	}
	defer renderer.Close(ctx)

	n, err := renderer.OpCount(ctx)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("no _initialize, no __wasm_call_ctors: ezu_op_count() = %d", n)
	if n != 0 {
		t.Errorf("op count %d: the constructors ran without being asked, so this module is not reactor-shaped", n)
	}
}

// The landmine the spike found: a host that neither runs `_initialize` nor
// calls `__wasm_call_ctors` gets a registry with zero ops and every style
// fails with an unhelpful "unknown op". Open does both, so both spellings
// must give the full registry.
func TestOpCountWithAndWithoutInitialize(t *testing.T) {
	ctx := context.Background()
	rt, err := NewRuntime(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer rt.Close(ctx)

	for _, tc := range []struct {
		name  string
		start startMode
	}{
		{"with _initialize", startBothNets},
		{"without start functions", startCtorsOnly},
	} {
		t.Run(tc.name, func(t *testing.T) {
			renderer, err := rt.instantiate(ctx, tc.name, tc.start)
			if err != nil {
				t.Fatal(err)
			}
			defer renderer.Close(ctx)

			n, err := renderer.OpCount(ctx)
			if err != nil {
				t.Fatal(err)
			}
			t.Logf("ezu_op_count() = %d (native = %d)", n, nativeOpCount)
			if n != nativeOpCount {
				t.Errorf("op count %d does not match the native %d", n, nativeOpCount)
			}

			names, err := renderer.OpNames(ctx)
			if err != nil {
				t.Fatal(err)
			}
			if len(names) != int(n) {
				t.Errorf("ezu_op_names listed %d names for a count of %d", len(names), n)
			}
		})
	}
}

// State that outlives a call is the whole reason for the reactor pin: a
// handle created by one call must still render through a later one, with
// unrelated calls in between.
func TestHandleSurvivesAcrossCalls(t *testing.T) {
	ctx, renderer := open(t, stainedGlassStyle)
	tile := readFile(t, "testdata/basemap-14-14554-6454.mvt")

	if err := renderer.BindSource(ctx, "basemap", tile, Bind{}); err != nil {
		t.Fatal(err)
	}
	// Unrelated traffic through the same instance, allocating and freeing
	// as it goes.
	for i := 0; i < 3; i++ {
		if _, err := renderer.OpNames(ctx); err != nil {
			t.Fatal(err)
		}
		if _, err := renderer.ParamsSchema(ctx); err != nil {
			t.Fatal(err)
		}
	}
	bound, err := renderer.BoundSources(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(bound) != 1 || bound[0] != "basemap" {
		t.Fatalf("bound sources %v after unrelated calls, want [basemap]", bound)
	}
	png, err := renderer.RenderTile(ctx, 14, 14554, 6454, Render{})
	if err != nil {
		t.Fatal(err)
	}
	if len(png) == 0 {
		t.Fatal("the render came back empty")
	}
}

func TestSIMDAndHeap(t *testing.T) {
	ctx, renderer := open(t, stainedGlassStyle)
	simd, err := renderer.SIMDEnabled(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if !simd {
		t.Error("the committed module was not built with +simd128, which is both faster and smaller")
	}
	heap, err := renderer.HeapBytes(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if heap == 0 {
		t.Error("the instance reports no linear memory at all")
	}
	t.Logf("simd=%v heap=%d bytes", simd, heap)
}
