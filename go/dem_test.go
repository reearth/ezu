package ezu

import (
	"strings"
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

const demZ, demX, demY = 10, 909, 402

func TestDEMRendersFromTheCentreAloneAndSaysWhatItIsMissing(t *testing.T) {
	ctx, renderer := open(t, demStyle)
	if err := renderer.InitLog(ctx, LogWarn); err != nil {
		t.Fatal(err)
	}
	if err := renderer.BindSource(ctx, demSource, readFile(t, demTile), Bind{}); err != nil {
		t.Fatal(err)
	}

	// Centre only: this renders — the stitch clamps the tile's own edge
	// into the pad — and that is exactly why it has to warn. A seam at the
	// tile border is not an error and would otherwise reach a caller as a
	// picture with nothing said about it.
	out, err := renderer.RenderTile(ctx, demZ, demX, demY, Render{})
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
		if err := renderer.BindSource(ctx, demSource, bytes, Bind{DX: o.DX, DY: o.DY}); err != nil {
			t.Fatalf("binding %+v: %v", o, err)
		}
	}
	if _, err := renderer.DrainLogs(ctx); err != nil {
		t.Fatal(err)
	}
	if _, err := renderer.RenderTile(ctx, demZ, demX, demY, Render{}); err != nil {
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

// Only the 3×3 is ever stitched or collided against, so an offset outside
// it would be accepted, stored and never read. It is refused instead, with
// the same name the JavaScript shell throws.
func TestBindRefusesAnOffsetOutsideTheNeighbourhood(t *testing.T) {
	ctx, renderer := open(t, demStyle)
	err := renderer.BindSource(ctx, demSource, readFile(t, demTile), Bind{DX: 2})
	if !IsKind(err, KindUnknownSrc) {
		t.Fatalf("binding at dx=2 gave %v, want UnknownSource", err)
	}
	if !strings.Contains(err.Error(), "3×3") {
		t.Errorf("the message does not say what the limit is: %v", err)
	}
}
