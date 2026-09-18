package ezu

import (
	"context"
	"errors"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tetratelabs/wazero"
)

// The concurrency rule is a property of the package's shape, not only of
// its documentation: a wasm instance has one allocator and no locks, so two
// goroutines in one renderer is memory corruption rather than a slow
// answer. The package refuses instead of serialising, so that a caller who
// wanted parallel tiles finds out here rather than in production.

func TestConcurrentEntryIsRefused(t *testing.T) {
	ctx, renderer := open(t, stainedGlassStyle)
	if err := renderer.BindSource(ctx, "basemap", readFile(t, "testdata/basemap-14-14554-6454.mvt"), Bind{}); err != nil {
		t.Fatal(err)
	}

	var refused atomic.Int32
	var succeeded atomic.Int32
	var wg sync.WaitGroup
	const goroutines = 4
	start := make(chan struct{})
	for i := 0; i < goroutines; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			<-start
			_, err := renderer.RenderTile(ctx, 14, 14554, 6454, Render{})
			switch {
			case err == nil:
				succeeded.Add(1)
			case errors.Is(err, ErrConcurrentUse):
				refused.Add(1)
			default:
				t.Errorf("unexpected error: %v", err)
			}
		}()
	}
	close(start)
	wg.Wait()

	// A render takes long enough that the other three meet the guard while
	// the first holds it, but the exact split is the scheduler's business.
	// What must hold is that nobody was let in alongside, and that nobody
	// got some third outcome.
	if succeeded.Load() < 1 {
		t.Error("every goroutine was refused; none of them was the first")
	}
	if refused.Load() < 1 {
		t.Error("nothing was refused; the guard did not engage at all")
	}
	if total := succeeded.Load() + refused.Load(); total != goroutines {
		t.Errorf("%d goroutines accounted for, want %d", total, goroutines)
	}
	t.Logf("%d succeeded, %d refused", succeeded.Load(), refused.Load())
}

// A refused call must have done nothing, so that a caller who recovers from
// one finds the renderer exactly as it was.
//
// The guard is taken directly rather than by racing a real call: what is
// being checked is the refusal's effect, and a test that has to win a race
// to check it would be reporting the scheduler rather than the package.
func TestARefusedCallLeavesNothingBehind(t *testing.T) {
	ctx, renderer := open(t, stainedGlassStyle)
	if err := renderer.BindSource(ctx, "basemap", readFile(t, "testdata/basemap-14-14554-6454.mvt"), Bind{}); err != nil {
		t.Fatal(err)
	}

	if !renderer.busy.CompareAndSwap(false, true) {
		t.Fatal("the renderer was already busy")
	}
	err := renderer.ClearSources(ctx)
	renderer.busy.Store(false)

	if !errors.Is(err, ErrConcurrentUse) {
		t.Fatalf("a call made while the guard was held gave %v, want ErrConcurrentUse", err)
	}
	bound, err := renderer.BoundSources(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(bound) != 1 {
		t.Errorf("bound sources %v after a refused ClearSources; the refusal had an effect", bound)
	}
}

// The answer to "I want tiles in parallel": several instances, one
// goroutine each.
func TestPoolRendersInParallel(t *testing.T) {
	ctx := context.Background()
	rt, err := NewRuntime(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer rt.Close(ctx)

	const n = 3
	pool, err := rt.NewPool(ctx, n, readFile(t, stainedGlassStyle))
	if err != nil {
		t.Fatal(err)
	}
	defer pool.Close(ctx)

	tile := readFile(t, "testdata/basemap-14-14554-6454.mvt")
	var wg sync.WaitGroup
	results := make([][]byte, 2*n)
	for i := range results {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			err := pool.Do(ctx, func(r *Renderer) error {
				if err := r.BindSource(ctx, "basemap", tile, Bind{}); err != nil {
					return err
				}
				out, err := r.RenderTile(ctx, 14, 14554, 6454, Render{})
				if err != nil {
					return err
				}
				results[i] = out
				return r.ClearSources(ctx)
			})
			if err != nil {
				t.Error(err)
			}
		}(i)
	}
	wg.Wait()

	// Every renderer in the pool must have produced the same tile: they are
	// separate instances of one deterministic renderer, and if which one
	// drew a tile could be told from the bytes, the pool would not be safe
	// to put behind a cache.
	for i, out := range results {
		if got := sha256hex(out); got != goldens[0].png {
			t.Errorf("result %d hashes %s, want %s", i, got, goldens[0].png)
		}
	}
}

func TestPoolNeedsAtLeastOne(t *testing.T) {
	ctx := context.Background()
	rt, err := NewRuntime(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer rt.Close(ctx)
	if _, err := rt.NewPool(ctx, 0, readFile(t, stainedGlassStyle)); err == nil {
		t.Error("a pool of 0 renderers was accepted")
	}
}

// Compiling this module is by far the most expensive thing the package
// does, and a cache is how a process stops paying for it on every start.
func TestCompilationCacheIsReused(t *testing.T) {
	ctx := context.Background()
	cache, err := wazero.NewCompilationCacheWithDir(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	defer cache.Close(ctx)

	style := readFile(t, stainedGlassStyle)
	tile := readFile(t, "testdata/basemap-14-14554-6454.mvt")

	var elapsed [2]time.Duration
	for i := range elapsed {
		start := time.Now()
		rt, err := NewRuntime(ctx, WithCompilationCache(cache))
		if err != nil {
			t.Fatal(err)
		}
		elapsed[i] = time.Since(start)

		// And a module out of the cache still renders the same tile, which
		// is the part worth checking: a cache that served something subtly
		// different would be worse than no cache.
		renderer, err := rt.NewRenderer(ctx, style)
		if err != nil {
			t.Fatal(err)
		}
		if err := renderer.BindSource(ctx, "basemap", tile, Bind{}); err != nil {
			t.Fatal(err)
		}
		out, err := renderer.RenderTile(ctx, 14, 14554, 6454, Render{})
		if err != nil {
			t.Fatal(err)
		}
		if got := sha256hex(out); got != goldens[0].png {
			t.Errorf("run %d hashes %s, want %s", i, got, goldens[0].png)
		}
		renderer.Close(ctx)
		rt.Close(ctx)
	}
	t.Logf("compile: cold %v, cached %v", elapsed[0], elapsed[1])
	if elapsed[1] >= elapsed[0] {
		t.Errorf("the cached start took %v against a cold %v; the cache bought nothing",
			elapsed[1], elapsed[0])
	}
}

// The runtime is the shared part: compiling once and instantiating many
// times is the whole reason it is separate from the renderers.
func TestOneRuntimeManyRenderers(t *testing.T) {
	ctx := context.Background()
	rt, err := NewRuntime(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer rt.Close(ctx)

	style := readFile(t, stainedGlassStyle)
	var renderers []*Renderer
	for i := 0; i < 3; i++ {
		r, err := rt.NewRenderer(ctx, style)
		if err != nil {
			t.Fatal(err)
		}
		defer r.Close(ctx)
		renderers = append(renderers, r)
	}
	// Separate linear memories: what one binds is invisible to the others.
	if err := renderers[0].BindSource(ctx, "basemap", readFile(t, "testdata/basemap-14-14554-6454.mvt"), Bind{}); err != nil {
		t.Fatal(err)
	}
	for i, r := range renderers {
		bound, err := r.BoundSources(ctx)
		if err != nil {
			t.Fatal(err)
		}
		want := 0
		if i == 0 {
			want = 1
		}
		if len(bound) != want {
			t.Errorf("renderer %d has %v bound, want %d source(s)", i, bound, want)
		}
	}
}
