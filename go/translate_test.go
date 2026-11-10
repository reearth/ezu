package ezu

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
)

// The same style crates/ezu-wasm/tests/translate.test.mjs translates, so
// the two shells are checked against one input.
const mapLibreStyle = `{
  "version": 8,
  "sources": {
    "basemap": {"type": "vector", "tiles": ["https://example.invalid/{z}/{x}/{y}.mvt"]}
  },
  "layers": [
    {"id": "bg", "type": "background", "paint": {"background-color": "#f0ead8"}},
    {"id": "water", "type": "fill", "source": "basemap", "source-layer": "water",
     "paint": {"fill-color": "#9cc0e0"}},
    {"id": "hidden", "type": "line", "source": "basemap", "source-layer": "roads",
     "layout": {"visibility": "none"}},
    {"id": "sky", "type": "sky"}
  ]
}`

func newTestRuntime(t *testing.T) (context.Context, *Runtime) {
	t.Helper()
	ctx := context.Background()
	rt, err := NewRuntime(ctx)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { rt.Close(ctx) })
	return ctx, rt
}

type recipeShape struct {
	TileSize uint32                     `json:"tile-size"`
	Pad      *uint32                    `json:"pad"`
	Nodes    map[string]json.RawMessage `json:"nodes"`
}

func TestTranslatedStyleBuildsARenderer(t *testing.T) {
	ctx, rt := newTestRuntime(t)
	got, err := rt.TranslateMapLibre(ctx, []byte(mapLibreStyle))
	if err != nil {
		t.Fatal(err)
	}
	var shape recipeShape
	if err := json.Unmarshal(got.Recipe, &shape); err != nil {
		t.Fatal(err)
	}
	if shape.TileSize != 512 {
		t.Errorf("tile-size %d, want 512", shape.TileSize)
	}
	if !strings.Contains(strings.Join(got.Warnings, "\n"), "sky") {
		t.Errorf("no warning for the sky layer in %q", got.Warnings)
	}

	renderer, err := rt.NewRenderer(ctx, got.Recipe)
	if err != nil {
		t.Fatal(err)
	}
	defer renderer.Close(ctx)
	if size, err := renderer.TileSize(ctx); err != nil || size != 512 {
		t.Errorf("the renderer's tile size is %d (%v), want 512", size, err)
	}
}

func TestTranslateOptionsReachTheTranslator(t *testing.T) {
	ctx, rt := newTestRuntime(t)
	plain, err := rt.TranslateMapLibre(ctx, []byte(mapLibreStyle))
	if err != nil {
		t.Fatal(err)
	}
	kept, err := rt.TranslateMapLibre(ctx, []byte(mapLibreStyle),
		TranslateTileSize(256), TranslatePad(0), TranslateKeepHidden())
	if err != nil {
		t.Fatal(err)
	}
	var p, k recipeShape
	if err := json.Unmarshal(plain.Recipe, &p); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(kept.Recipe, &k); err != nil {
		t.Fatal(err)
	}
	if k.TileSize != 256 {
		t.Errorf("tile-size %d, want 256", k.TileSize)
	}
	if k.Pad == nil || *k.Pad != 0 {
		t.Errorf("pad %v, want 0", k.Pad)
	}
	if len(k.Nodes) <= len(p.Nodes) {
		t.Errorf("TranslateKeepHidden kept %d nodes, no more than the %d without it",
			len(k.Nodes), len(p.Nodes))
	}
}

func TestTranslateRefusesWithInvalidStyle(t *testing.T) {
	ctx, rt := newTestRuntime(t)
	for _, tc := range []struct {
		style string
		opts  []TranslateOption
	}{
		{"not json", nil},
		{`{"version": 8, "sources": {}}`, nil},
		{mapLibreStyle, []TranslateOption{TranslateTileSize(0)}},
	} {
		_, err := rt.TranslateMapLibre(ctx, []byte(tc.style), tc.opts...)
		if !IsKind(err, KindInvalidStyle) {
			t.Errorf("translating %.20q: got %v, want InvalidStyle", tc.style, err)
		}
	}
}
