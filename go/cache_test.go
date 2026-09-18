package ezu

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	"github.com/tetratelabs/wazero"
)

// A compilation cache saves time and nothing else, so no way it can fail is
// allowed to stop a runtime being built. wazero does not see it that way on
// its own: a miss it cannot write, and an entry it cannot read, both come
// back as errors from compilation, and a service pointed at a read-only
// mount or handed a half-copied image layer would not start at all.
//
// These are the two shapes that produced that, and what each one must do
// now instead: build the runtime the slow way and say so.

// A cache directory nothing can be written to, which is what a read-only
// mount is, and a miss in it — which is what a rebuilt module or an
// upgraded wazero produces.
func TestAnUnwritableCacheDirDoesNotStopStartup(t *testing.T) {
	if os.Geteuid() == 0 {
		t.Skip("running as root: file permissions do not apply, so there is nothing to make unwritable")
	}
	ctx := context.Background()
	dir := t.TempDir()

	// Building a cache over the directory once makes the versioned
	// subdirectory the entries live in; taking write permission off both is
	// what leaves a miss with nowhere to go.
	cache, err := wazero.NewCompilationCacheWithDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	cache.Close(ctx)

	sub := onlySubdir(t, dir)
	readOnly(t, sub)
	readOnly(t, dir)

	rt, err := NewRuntime(ctx, WithCompilationCacheDir(dir))
	if err != nil {
		t.Fatalf("a cache that cannot be written failed the start: %v", err)
	}
	defer rt.Close(ctx)

	if rt.CompilationCacheError() == nil {
		t.Error("the runtime compiled without its cache and did not say so, which leaves a slow " +
			"start as the only symptom")
	} else {
		t.Logf("reported: %v", rt.CompilationCacheError())
	}
	mustRender(t, ctx, rt)
}

// A damaged entry: a truncated copy, an ENOSPC during a write, a bad block.
// wazero's checksum catches it and then declines to recompile over it.
func TestADamagedCacheEntryDoesNotStopStartup(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()

	// Fill the cache the ordinary way, so the entry is a real one.
	rt, err := NewRuntime(ctx, WithCompilationCacheDir(dir))
	if err != nil {
		t.Fatal(err)
	}
	if rt.CompilationCacheError() != nil {
		t.Fatalf("a fresh writable cache should just work: %v", rt.CompilationCacheError())
	}
	rt.Close(ctx)

	entry := onlyEntry(t, onlySubdir(t, dir))
	body, err := os.ReadFile(entry)
	if err != nil {
		t.Fatal(err)
	}
	if len(body) < 1024 {
		t.Fatalf("%s is %d bytes, which is not the compiled module — this test is looking in the "+
			"wrong place", entry, len(body))
	}
	if err := os.WriteFile(entry, body[:len(body)/2], 0o600); err != nil {
		t.Fatal(err)
	}

	again, err := NewRuntime(ctx, WithCompilationCacheDir(dir))
	if err != nil {
		t.Fatalf("a truncated cache entry failed the start: %v", err)
	}
	defer again.Close(ctx)

	if again.CompilationCacheError() == nil {
		t.Error("the truncated entry was not reported, so nothing distinguishes this start from a " +
			"fast one except its duration")
	} else {
		t.Logf("reported: %v", again.CompilationCacheError())
	}
	mustRender(t, ctx, again)
}

// The other side of it: a cache that works, and no cache at all, both report
// nothing. Otherwise a host logging the fallback would log it every start.
func TestAUsableCacheReportsNothing(t *testing.T) {
	ctx := context.Background()

	plain, err := NewRuntime(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer plain.Close(ctx)
	if plain.CompilationCacheError() != nil {
		t.Errorf("no cache was asked for, yet one was reported: %v", plain.CompilationCacheError())
	}

	dir := t.TempDir()

	// Cold, then warm off the entry the cold one wrote.
	for _, when := range []string{"cold", "warm"} {
		rt, err := NewRuntime(ctx, WithCompilationCacheDir(dir))
		if err != nil {
			t.Fatalf("%s: %v", when, err)
		}
		if rt.CompilationCacheError() != nil {
			t.Errorf("%s: %v", when, rt.CompilationCacheError())
		}
		rt.Close(ctx)
	}
}

// The third shape: a path that cannot hold a cache at all, so the failure
// happens before anything is compiled. Same answer as the other two.
func TestACacheDirThatCannotExistDoesNotStopStartup(t *testing.T) {
	ctx := context.Background()
	notADir := filepath.Join(t.TempDir(), "occupied")
	if err := os.WriteFile(notADir, []byte("a file where a directory was asked for"), 0o600); err != nil {
		t.Fatal(err)
	}

	rt, err := NewRuntime(ctx, WithCompilationCacheDir(notADir))
	if err != nil {
		t.Fatalf("a cache directory that cannot exist failed the start: %v", err)
	}
	defer rt.Close(ctx)

	if rt.CompilationCacheError() == nil {
		t.Error("the cache directory could not be made and nothing said so")
	} else {
		t.Logf("reported: %v", rt.CompilationCacheError())
	}
	mustRender(t, ctx, rt)
}

// mustRender checks that a runtime built after a cache was thrown away is a
// working one and not merely a value: the module compiled, the constructors
// ran, and a style renders.
func mustRender(t *testing.T, ctx context.Context, rt *Runtime) {
	t.Helper()
	renderer, err := rt.NewRenderer(ctx, readFile(t, stainedGlassStyle))
	if err != nil {
		t.Fatal(err)
	}
	defer renderer.Close(ctx)
	if err := renderer.BindSource(ctx, "basemap", readFile(t, "testdata/basemap-14-14554-6454.mvt"), Bind{}); err != nil {
		t.Fatal(err)
	}
	out, err := renderer.RenderTile(ctx, 14, 14554, 6454, Render{})
	if err != nil {
		t.Fatal(err)
	}
	if got := sha256hex(out); got != goldens[0].png {
		t.Errorf("the tile hashes %s, want %s", got, goldens[0].png)
	}
}

// onlySubdir is the single wazero-<version>-<arch>-<os> directory a cache
// makes for itself when it is constructed.
func onlySubdir(t *testing.T, dir string) string {
	t.Helper()
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	var found []string
	for _, e := range entries {
		if e.IsDir() {
			found = append(found, filepath.Join(dir, e.Name()))
		}
	}
	if len(found) != 1 {
		t.Fatalf("%s holds %d directories, want the one wazero makes: %v", dir, len(found), found)
	}
	return found[0]
}

// onlyEntry is the single compiled-module file in a cache directory.
func onlyEntry(t *testing.T, dir string) string {
	t.Helper()
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	var found []string
	for _, e := range entries {
		if !e.IsDir() {
			found = append(found, filepath.Join(dir, e.Name()))
		}
	}
	if len(found) != 1 {
		t.Fatalf("%s holds %d files, want the one compiled module: %v", dir, len(found), found)
	}
	return found[0]
}

// readOnly takes write permission off a directory and puts it back when the
// test ends, so that the temp directory can still be cleaned up.
func readOnly(t *testing.T, dir string) {
	t.Helper()
	info, err := os.Stat(dir)
	if err != nil {
		t.Fatal(err)
	}
	mode := info.Mode().Perm()
	if err := os.Chmod(dir, mode&^0o222); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.Chmod(dir, mode) })
}
