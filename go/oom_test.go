package ezu

import (
	"context"
	"testing"
)

// Heap exhaustion, which is the failure a long-running tile service will
// actually meet, and the one that says nothing useful if nobody arranges
// for it to.
//
// Left alone, a Rust allocation that cannot be served traps the instance on
// `unreachable`: the host gets "wasm error: unreachable" and no hint that
// the cause was memory, let alone how much was wanted. The module's global
// allocator calls back out to ezu_host.oom instead, which the host answers
// by recording the size and trapping deliberately — so the same dead
// instance comes back named, with a number.
//
// The instance is finished either way. What this buys is diagnosis, not
// recovery, and the test holds both halves of that.
func TestOutOfMemoryIsNamedAndEndsTheRenderer(t *testing.T) {
	ctx := context.Background()
	// Small enough that a 512 px render through this style cannot fit, and
	// large enough that the module instantiates and the style builds. There
	// is nothing magic about the number: it is "less than this style
	// needs", which the test then checks it really was.
	rt, err := NewRuntime(ctx, WithMemoryLimit(12<<20))
	if err != nil {
		t.Fatal(err)
	}
	defer rt.Close(ctx)

	renderer, err := rt.NewRenderer(ctx, readFile(t, stainedGlassStyle))
	if err != nil {
		t.Fatal(err)
	}
	defer renderer.Close(ctx)
	if err := renderer.BindSource(ctx, "basemap", readFile(t, "testdata/basemap-14-14554-6454.mvt")); err != nil {
		t.Fatal(err)
	}

	_, err = renderer.RenderTile(ctx, Tile{Z: 14, X: 14554, Y: 6454}, Render{})
	if err == nil {
		t.Skip("this style rendered inside the cap; nothing to say about running out")
	}
	if !IsKind(err, KindOutOfMemory) {
		t.Fatalf("running out of memory gave %v; want an OutOfMemory error, which is what the "+
			"ezu_host.oom import exists to produce", err)
	}
	e := err.(*Error)
	if e.Bytes == 0 {
		t.Error("the error carries no requested size, so the host learned nothing about how short it was")
	}
	t.Logf("%v", err)

	// And the instance is finished: every later call says so rather than
	// reading whatever is left in a linear memory that was abandoned
	// mid-allocation.
	if _, err := renderer.RenderTile(ctx, Tile{Z: 14, X: 14554, Y: 6454}, Render{}); err != ErrRendererDead {
		t.Errorf("a call after the trap gave %v, want ErrRendererDead", err)
	}
	if _, err := renderer.BoundSources(ctx); err != ErrRendererDead {
		t.Errorf("a query after the trap gave %v, want ErrRendererDead", err)
	}
}

// A limit generous enough for the style is not in the way: the same tile,
// the same bytes.
func TestAGenerousMemoryLimitChangesNothing(t *testing.T) {
	ctx := context.Background()
	rt, err := NewRuntime(ctx, WithMemoryLimit(512<<20))
	if err != nil {
		t.Fatal(err)
	}
	defer rt.Close(ctx)

	renderer, err := rt.NewRenderer(ctx, readFile(t, stainedGlassStyle))
	if err != nil {
		t.Fatal(err)
	}
	defer renderer.Close(ctx)
	if err := renderer.BindSource(ctx, "basemap", readFile(t, "testdata/basemap-14-14554-6454.mvt")); err != nil {
		t.Fatal(err)
	}
	out, err := renderer.RenderTile(ctx, Tile{Z: 14, X: 14554, Y: 6454}, Render{})
	if err != nil {
		t.Fatal(err)
	}
	if got := sha256hex(out); got != goldens[0].png {
		t.Errorf("under a memory limit the tile hashes %s, want %s", got, goldens[0].png)
	}
}

// A renderer that trapped is not handed back to the next caller: the pool
// closes it and shrinks, rather than turning one failed render into every
// later one.
func TestPoolDropsATrappedRenderer(t *testing.T) {
	ctx := context.Background()
	rt, err := NewRuntime(ctx, WithMemoryLimit(12<<20))
	if err != nil {
		t.Fatal(err)
	}
	defer rt.Close(ctx)

	pool, err := rt.NewPool(ctx, 2, readFile(t, stainedGlassStyle))
	if err != nil {
		t.Fatal(err)
	}
	defer pool.Close(ctx)

	renderer, err := pool.Acquire(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if err := renderer.BindSource(ctx, "basemap", readFile(t, "testdata/basemap-14-14554-6454.mvt")); err != nil {
		t.Fatal(err)
	}
	if _, err := renderer.RenderTile(ctx, Tile{Z: 14, X: 14554, Y: 6454}, Render{}); err == nil {
		t.Skip("this style rendered inside the cap")
	}
	pool.Release(renderer)

	// The surviving renderer is still usable, and the dead one is gone.
	next, err := pool.Acquire(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer pool.Release(next)
	if next == renderer {
		t.Fatal("the pool handed back the renderer that trapped")
	}
	if _, err := next.BoundSources(ctx); err != nil {
		t.Errorf("the surviving renderer is unusable: %v", err)
	}
}
