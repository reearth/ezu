package ezu

import (
	"context"
	"errors"
	"fmt"
)

// Pool is a fixed set of renderers over one style, for rendering tiles in
// parallel.
//
// A wasm instance is single-threaded, so parallelism here means several
// instances and not several goroutines in one — see the package
// documentation. A Pool is the small amount of bookkeeping that makes that
// the easy thing to do: N renderers behind a channel, one goroutine holding
// one at a time, and the concurrency rule satisfied by construction.
//
// Size it by memory rather than by CPU. Every renderer is a full linear
// memory with its own copies of whatever it has been given — glyphs,
// sprites, its own render cache — so N renderers cost roughly N times one
// renderer's [Renderer.MemoryUsage], and that is usually the binding
// constraint long before cores are.
//
// The persistent banks are per renderer, not per pool: a brush, font,
// sprite or glyph range bound to one is unknown to the others. Bind what a
// style needs to each of them, or bind per tile — [Renderer.NeededCodepoints]
// names exactly what a tile wants, so the second is cheaper than it sounds.
type Pool struct {
	free chan *Renderer
	all  []*Renderer
}

// NewPool builds n renderers over the same style.
func (r *Runtime) NewPool(ctx context.Context, n int, styleJSON []byte) (*Pool, error) {
	if n < 1 {
		return nil, fmt.Errorf("ezu: a pool needs at least one renderer, got %d", n)
	}
	pool := &Pool{free: make(chan *Renderer, n)}
	for i := 0; i < n; i++ {
		renderer, err := r.NewRenderer(ctx, styleJSON)
		if err != nil {
			pool.Close(ctx)
			return nil, err
		}
		pool.all = append(pool.all, renderer)
		pool.free <- renderer
	}
	return pool, nil
}

// Acquire takes a renderer out of the pool, waiting until one is free or
// ctx is done. Release it when finished — a renderer that is never released
// is one the pool has permanently lost.
//
//	r, err := pool.Acquire(ctx)
//	if err != nil { return err }
//	defer pool.Release(r)
func (p *Pool) Acquire(ctx context.Context) (*Renderer, error) {
	select {
	case renderer := <-p.free:
		return renderer, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

// Release returns a renderer to the pool.
//
// A renderer whose instance has trapped is not returned: it is closed and
// the pool shrinks by one. That is the honest outcome — the instance's
// memory means nothing after a trap, and silently handing it to the next
// caller would turn one failed render into every later one. A pool that has
// shrunk to nothing makes [Pool.Acquire] block, which is the signal to
// rebuild it.
func (p *Pool) Release(r *Renderer) {
	if r == nil {
		return
	}
	if r.dead.Load() {
		_ = r.Close(context.Background())
		return
	}
	p.free <- r
}

// Do acquires a renderer, runs f with it, and releases it — the form most
// callers want.
func (p *Pool) Do(ctx context.Context, f func(*Renderer) error) error {
	renderer, err := p.Acquire(ctx)
	if err != nil {
		return err
	}
	defer p.Release(renderer)
	return f(renderer)
}

// Close closes every renderer in the pool, whether or not it is checked
// out. Do not use a pool, or anything acquired from it, afterwards.
func (p *Pool) Close(ctx context.Context) error {
	var errs []error
	for _, renderer := range p.all {
		if err := renderer.Close(ctx); err != nil {
			errs = append(errs, err)
		}
	}
	p.all = nil
	return errors.Join(errs...)
}
