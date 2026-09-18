package ezu

import (
	"context"
	"log/slog"
	"strings"
	"sync"
	"testing"
)

// A dem source is the one kind that binds as a *stitched* canvas rather
// than as a tile: its 3×3 is decoded and sewn into one buffer so that
// anything sampling the pad reads real elevation rather than a clamped
// edge. That makes two things worth holding here — that binding by
// neighbour offset works at all, and that a window short of what the source
// asked for is said out loud rather than quietly seaming.
const (
	demStyle  = hillshadeStyle
	demSource = "terrain"
	demTile   = "testdata/terrain-10-909-402.webp"
)

var demCoord = Tile{Z: 10, X: 909, Y: 402}

func TestDEMRendersFromTheCentreAloneAndSaysWhatItIsMissing(t *testing.T) {
	ctx, renderer := open(t, demStyle)
	if err := renderer.InitLog(ctx, LogWarn); err != nil {
		t.Fatal(err)
	}
	if err := renderer.BindSource(ctx, demSource, readFile(t, demTile)); err != nil {
		t.Fatal(err)
	}

	// Centre only: this renders — the stitch clamps the tile's own edge
	// into the pad — and that is exactly why it has to warn. A seam at the
	// tile border is not an error and would otherwise reach a caller as a
	// picture with nothing said about it.
	out, err := renderer.RenderTile(ctx, demCoord, Render{})
	if err != nil {
		t.Fatal(err)
	}
	if len(out) == 0 {
		t.Fatal("the render came back empty")
	}

	lines, err := renderer.DrainLogs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	var warned bool
	for _, l := range lines {
		t.Logf("  %s", l)
		if strings.Contains(l, demSource) && strings.Contains(l, "unbound") {
			warned = true
		}
	}
	if !warned {
		t.Errorf("a centre-only bind rendered without warning about the 8 neighbours it asked for; lines: %v", lines)
	}

	// Draining empties the buffer, so a host that drains per tile does not
	// re-read the previous tile's lines and attribute them to this one.
	again, err := renderer.DrainLogs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(again) != 0 {
		t.Errorf("a second drain returned %d lines; the first did not empty the buffer", len(again))
	}
}

func TestDEMWithEveryOffsetBoundIsSilent(t *testing.T) {
	ctx, renderer := open(t, demStyle)
	if err := renderer.InitLog(ctx, LogWarn); err != nil {
		t.Fatal(err)
	}
	// The same bytes at all nine offsets. Geographically it is nonsense —
	// what it checks is the plumbing: that Bind's coord reaches the
	// renderer, and that a fully bound window stops the warning. The
	// picture is not asserted for that reason.
	offsets, err := renderer.RequestedNeighborOffsets(ctx, demSource)
	if err != nil {
		t.Fatal(err)
	}
	bytes := readFile(t, demTile)
	for _, o := range append(offsets, Offset{0, 0}) {
		if err := renderer.BindSource(ctx, demSource, bytes, AtOffset(o)); err != nil {
			t.Fatalf("binding %+v: %v", o, err)
		}
	}
	if _, err := renderer.DrainLogs(ctx); err != nil {
		t.Fatal(err)
	}
	if _, err := renderer.RenderTile(ctx, demCoord, Render{}); err != nil {
		t.Fatal(err)
	}
	lines, err := renderer.DrainLogs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	for _, l := range lines {
		if strings.Contains(l, "unbound") {
			t.Errorf("a fully bound 3×3 still warned: %s", l)
		}
	}
}

// A tile that is wrong rather than missing has to announce itself, and the
// pull API only announces it to a host that thought to ask. A logger on the
// runtime is what makes the warning arrive unasked, from every renderer it
// makes — including the ones inside a pool, which would otherwise each need
// their own InitLog and their own drain.
func TestALoggerOnTheRuntimeHearsWarningsWithoutBeingAsked(t *testing.T) {
	ctx := context.Background()
	sink := &recordingHandler{}
	rt, err := NewRuntime(ctx, WithLogger(slog.New(sink)))
	if err != nil {
		t.Fatal(err)
	}
	defer rt.Close(ctx)

	pool, err := rt.NewPool(ctx, 2, readFile(t, demStyle))
	if err != nil {
		t.Fatal(err)
	}
	defer pool.Close(ctx)

	// Centre only, so the stitch clamps the tile's own edge into the pad and
	// the renderer warns about the eight it asked for and did not get.
	err = pool.Do(ctx, func(r *Renderer) error {
		if err := r.BindSource(ctx, demSource, readFile(t, demTile)); err != nil {
			return err
		}
		_, err := r.RenderTile(ctx, demCoord, Render{})
		return err
	})
	if err != nil {
		t.Fatal(err)
	}

	var warned bool
	for _, rec := range sink.records() {
		t.Logf("  %s %s", rec.Level, rec.Message)
		if rec.Level == slog.LevelWarn && strings.Contains(rec.Message, "unbound") {
			warned = true
		}
	}
	if !warned {
		t.Error("nothing was logged about the unbound neighbours; a seamed tile came back silently")
	}

	// And the lines say which instance drew the tile, which is the only way
	// to read a pool's output.
	for _, rec := range sink.records() {
		var named bool
		rec.Attrs(func(a slog.Attr) bool {
			if a.Key == "renderer" && a.Value.String() != "" {
				named = true
			}
			return true
		})
		if !named {
			t.Errorf("a line carried no renderer attribute: %s", rec.Message)
		}
	}
}

// recordingHandler keeps every record for the test to look at. A pool means
// several goroutines may be logging, so it locks.
type recordingHandler struct {
	mu   sync.Mutex
	seen []slog.Record
}

func (h *recordingHandler) Enabled(context.Context, slog.Level) bool { return true }

func (h *recordingHandler) Handle(_ context.Context, r slog.Record) error {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.seen = append(h.seen, r)
	return nil
}

func (h *recordingHandler) WithAttrs([]slog.Attr) slog.Handler { return h }
func (h *recordingHandler) WithGroup(string) slog.Handler      { return h }

func (h *recordingHandler) records() []slog.Record {
	h.mu.Lock()
	defer h.mu.Unlock()
	return append([]slog.Record(nil), h.seen...)
}

// Only the 3×3 is ever stitched or collided against, so an offset outside
// it would be accepted, stored and never read. It is refused instead, with
// the same name the JavaScript shell throws.
func TestBindRefusesAnOffsetOutsideTheNeighbourhood(t *testing.T) {
	ctx, renderer := open(t, demStyle)
	err := renderer.BindSource(ctx, demSource, readFile(t, demTile), AtOffset(Offset{DX: 2}))
	if !IsKind(err, KindUnknownSrc) {
		t.Fatalf("binding at dx=2 gave %v, want UnknownSource", err)
	}
	if !strings.Contains(err.Error(), "3×3") {
		t.Errorf("the message does not say what the limit is: %v", err)
	}
}
