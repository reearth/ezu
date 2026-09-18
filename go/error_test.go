package ezu

import (
	"context"
	"errors"
	"strings"
	"testing"
)

// The error names are a contract shared with ezu's JavaScript bindings: the
// same failure has the same name in both, so handling written for one host
// is handling for the other. What is checked here is that each name
// survives the trip out through a flat ABI — the name lives in the module's
// last-error slot, not in the return code, and a shell that dropped it
// would still look like it worked.
//
// The counterpart on the JavaScript side is the list in
// crates/ezu-wasm/src/lib.rs, and the kinds themselves are
// ezu_renderer::ErrorKind, whose own test pins the strings.
func TestFailuresCarryTheirNames(t *testing.T) {
	ctx := context.Background()
	rt, err := NewRuntime(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer rt.Close(ctx)

	style := readFile(t, stainedGlassStyle)

	t.Run("InvalidStyle", func(t *testing.T) {
		if _, err := rt.NewRenderer(ctx, []byte("{ this is not json")); !IsKind(err, KindInvalidStyle) {
			t.Fatalf("a malformed style gave %v, want InvalidStyle", err)
		}
		// A document that parses but names an op that does not exist is the
		// same kind, and the message is the part a human needs.
		_, err := rt.NewRenderer(ctx, []byte(`{"name":"x","sources":{},"nodes":{"a":{"op":"nope"}},"output":"@a"}`))
		if !IsKind(err, KindInvalidStyle) {
			t.Fatalf("an unknown op gave %v, want InvalidStyle", err)
		}
		if !strings.Contains(err.Error(), "nope") {
			t.Errorf("the message does not name the op: %v", err)
		}
	})

	t.Run("UnknownSource", func(t *testing.T) {
		renderer, err := rt.NewRenderer(ctx, style)
		if err != nil {
			t.Fatal(err)
		}
		defer renderer.Close(ctx)
		err = renderer.BindSource(ctx, "not-in-the-style", []byte("x"))
		if !IsKind(err, KindUnknownSrc) {
			t.Fatalf("binding an undeclared source gave %v, want UnknownSource", err)
		}
	})

	t.Run("MvtDecode", func(t *testing.T) {
		renderer, err := rt.NewRenderer(ctx, style)
		if err != nil {
			t.Fatal(err)
		}
		defer renderer.Close(ctx)
		// A vector source validates its payload at bind time, where the
		// host still knows which fetch produced it.
		err = renderer.BindSource(ctx, "basemap", []byte("definitely not a vector tile"))
		if !IsKind(err, "MvtDecode") {
			t.Fatalf("binding rubbish as MVT gave %v, want MvtDecode", err)
		}
	})

	t.Run("DemDecode", func(t *testing.T) {
		renderer, err := rt.NewRenderer(ctx, readFile(t, hillshadeStyle))
		if err != nil {
			t.Fatal(err)
		}
		defer renderer.Close(ctx)
		// A DEM payload is decoded at render time, once the tile id is
		// known, so this is the kind that surfaces from the render rather
		// than from the bind.
		if err := renderer.BindSource(ctx, "terrain", []byte("not an image")); err != nil {
			t.Fatal(err)
		}
		_, err = renderer.RenderTile(ctx, demCoord, Render{})
		if !IsKind(err, "DemDecode") {
			t.Fatalf("rendering from rubbish DEM bytes gave %v, want DemDecode", err)
		}
	})

	t.Run("option errors match the JavaScript shell", func(t *testing.T) {
		renderer, err := rt.NewRenderer(ctx, style)
		if err != nil {
			t.Fatal(err)
		}
		defer renderer.Close(ctx)
		// The JS shell refuses an unrecognised format with InvalidStyle
		// rather than quietly answering PNG, and so does this one.
		if _, err := renderer.RenderTile(ctx, Tile{Z: 14, X: 14554, Y: 6454}, Render{Format: "jpeg"}); !IsKind(err, KindInvalidStyle) {
			t.Errorf("an unknown format gave %v, want InvalidStyle", err)
		}
		if _, err := renderer.RenderTile(ctx, Tile{Z: 14, X: 14554, Y: 6454}, Render{PNGCompression: "maximum"}); !IsKind(err, KindInvalidStyle) {
			t.Errorf("an unknown png compression gave %v, want InvalidStyle", err)
		}
	})
}

// The error carries a message as well as a name, and the message is the
// detail alone — the name lives in its own field and must not appear twice.
func TestErrorShape(t *testing.T) {
	ctx := context.Background()
	rt, err := NewRuntime(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer rt.Close(ctx)

	_, err = rt.NewRenderer(ctx, []byte("{ nope"))
	var e *Error
	if !errors.As(err, &e) {
		t.Fatalf("%v is not an *ezu.Error", err)
	}
	if e.Name == "" || e.Message == "" {
		t.Fatalf("name %q, message %q: both must be set", e.Name, e.Message)
	}
	if strings.Contains(e.Message, e.Name) {
		t.Errorf("the message repeats the name: %q", e.Message)
	}
	if e.Code >= 0 {
		t.Errorf("code %d: a failure's code is negative", e.Code)
	}
	if !strings.Contains(e.Error(), e.Name) || !strings.Contains(e.Error(), e.Message) {
		t.Errorf("Error() %q drops one of the two", e.Error())
	}
}
