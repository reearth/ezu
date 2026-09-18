# ezu-renderer

The embedding renderer, with no host in it.

An *embedding* is ezu running somewhere that owns its own I/O: a browser
tab, a Workers isolate, a Go process driving a wasm runtime. Such a host
cannot let the renderer fetch anything, so the flow inverts — the host
fetches, binds each source's bytes, and asks for one tile:

```rust
use ezu_renderer::{BindOptions, RenderOptions, Renderer};

let mut r = Renderer::new(style_json)?;
r.bind_source("basemap", mvt_bytes, &BindOptions::default())?;
let png = r.render_tile(12, 1, 2, RenderOptions::default())?;
r.clear_sources();
```

That flow — dispatching on each source's declared type, stitching a DEM's
3×3 window, resolving overzoom per neighbour, the glyph and neighbour
prepasses a host needs in order to know what to fetch, and the names its
failures carry — is the same whatever language is calling. It lives here
so that each shell over it is only a translation: read the host's options,
build the host's result, convert the error.

`ezu-wasm` is the first such shell. Because the failure names come from
[`ErrorKind`](src/error.rs) rather than from any one shell, a JS caller
branching on `e.name` and a caller in another language branching on the
same string see the same failure under the same name.

Not published to crates.io: the shells are not published either, and the
API is free to move until more than one of them has been written against
it. Native Rust callers want [`ezu`](../ezu) — it fetches for you.
