package ezu

import (
	"context"
	"encoding/json"
	"fmt"
)

// TranslateOption qualifies one call to [Runtime.TranslateMapLibre]. With
// none of them, the style is translated with the defaults `ezu translate`
// has: 512-pixel tiles, a pad of 16, hidden layers dropped, legacy forms
// left as they are, and no font mapping.
//
// Options rather than a struct because a pad of zero is a real pad, and a
// struct would need a zero that means "the default".
type TranslateOption func(*translateOptions)

type translateOptions struct {
	tileSize    uint32
	hasTileSize bool
	pad         uint32
	hasPad      bool
	keepHidden  bool
	migrate     bool
	fonts       map[string]string
}

// TranslateTileSize sets the translated style's tile-size. MapLibre draws
// 512-pixel tiles, which is the default.
func TranslateTileSize(n uint32) TranslateOption {
	return func(o *translateOptions) { o.tileSize, o.hasTileSize = n, true }
}

// TranslatePad sets the translated style's pad: the margin a stroke
// running just outside the tile is painted into, so the part of it inside
// the tile is not lost. The default, 16, covers what a typical basemap
// draws.
func TranslatePad(n uint32) TranslateOption {
	return func(o *translateOptions) { o.pad, o.hasPad = n, true }
}

// TranslateKeepHidden keeps the style's `visibility: "none"` layers,
// switched off behind a switch node, instead of dropping them.
func TranslateKeepHidden() TranslateOption {
	return func(o *translateOptions) { o.keepHidden = true }
}

// TranslateMigrate converts the style's legacy forms — `stops` function
// objects, `{token}` strings — to expressions before translating it, as
// MapLibre's own migrate does. A style already written in expressions
// needs nothing.
func TranslateMigrate() TranslateOption {
	return func(o *translateOptions) { o.migrate = true }
}

// TranslateFont renders the `text-font` stack entry name from source — an
// installed font (`system:Helvetica`) or a font file's URL — instead of
// the style's glyphs endpoint. Repeat it for each entry; it is the CLI's
// `--font NAME=SOURCE`.
func TranslateFont(name, source string) TranslateOption {
	return func(o *translateOptions) {
		if o.fonts == nil {
			o.fonts = map[string]string{}
		}
		o.fonts[name] = source
	}
}

func (o translateOptions) json() ([]byte, error) {
	payload := map[string]any{}
	if o.hasTileSize {
		payload["tileSize"] = o.tileSize
	}
	if o.hasPad {
		payload["pad"] = o.pad
	}
	if o.keepHidden {
		payload["keepHidden"] = true
	}
	if o.migrate {
		payload["migrate"] = true
	}
	if len(o.fonts) > 0 {
		payload["fonts"] = o.fonts
	}
	if len(payload) == 0 {
		return nil, nil
	}
	return json.Marshal(payload)
}

// Translation is a translated MapLibre style.
type Translation struct {
	// Recipe is the ezu style, ready for [Runtime.NewRenderer].
	Recipe json.RawMessage `json:"recipe"`
	// Warnings are what the translator skipped or approximated, one line
	// each.
	Warnings []string `json:"warnings"`
}

// TranslateMapLibre translates a MapLibre GL style into an ezu style, as
// `ezu translate` does — the JS package's translateMapLibre. A style that
// is not JSON, or that the translator refuses, is an [Error] named
// InvalidStyle.
//
// It needs no renderer, and builds a wasm instance of its own for the call;
// it is safe for concurrent use, as the runtime is.
func (r *Runtime) TranslateMapLibre(ctx context.Context, styleJSON []byte, opts ...TranslateOption) (Translation, error) {
	var o translateOptions
	for _, opt := range opts {
		opt(&o)
	}
	optsJSON, err := o.json()
	if err != nil {
		return Translation{}, err
	}

	r.mu.Lock()
	r.nextName++
	name := fmt.Sprintf("ezu-%d", r.nextName)
	r.mu.Unlock()
	instance, err := r.instantiate(ctx, name, startBothNets)
	if err != nil {
		return Translation{}, err
	}
	defer instance.Close(ctx)

	_, payload, err := instance.withSlots(ctx, func(slots uint32) (int64, error) {
		stylePtr, err := instance.writeBytes(ctx, styleJSON)
		if err != nil {
			return 0, err
		}
		defer instance.free(ctx, stylePtr, uint32(len(styleJSON)))
		optsPtr, err := instance.writeBytes(ctx, optsJSON)
		if err != nil {
			return 0, err
		}
		defer instance.free(ctx, optsPtr, uint32(len(optsJSON)))
		return instance.call1(ctx, "ezu_translate_maplibre",
			uint64(stylePtr), uint64(len(styleJSON)),
			uint64(optsPtr), uint64(len(optsJSON)),
			uint64(slots), uint64(slots+4))
	})
	if err != nil {
		return Translation{}, err
	}
	var out Translation
	if err := json.Unmarshal(payload, &out); err != nil {
		return Translation{}, fmt.Errorf("ezu: reading the translation: %w", err)
	}
	return out, nil
}
