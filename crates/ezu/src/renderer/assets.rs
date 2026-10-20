//! Letting the renderer read a style's assets itself, for a native host.
//!
//! Everywhere else the renderer does no I/O: the host fetches each source
//! and binds the bytes. That is the only shape a browser or a wasm runtime
//! can offer, and the JS and Go packages offer exactly it. A native Rust
//! host can do more, and `ezu` itself is one: it reads brushes and images
//! from beside the style, resolves `system:` fonts from the operating
//! system, and fetches glyph ranges as labels turn out to need them, in the
//! middle of a render. [`Renderer::resolve_assets`] hands the renderer that
//! ability, so such a host renders through the same calls as every other
//! instead of keeping its own render path.

use std::path::Path;

use super::{err, Error, ErrorKind, Renderer};

impl Renderer {
    /// Read or fetch every document-scoped source the style declares —
    /// brushes, images, sprites, fonts and glyphs — the way the `ezu` CLI
    /// does, rather than having the host bind each one.
    ///
    /// `file:` paths resolve against `base_dir`, `http(s)://` URLs are
    /// fetched, `data:` URLs decoded and `system:` fonts looked up in the
    /// operating system's fonts. Glyphs are not read here at all: each
    /// glyphs source gets a stack that reads or fetches a 256-codepoint
    /// range the first time a label asks for a glyph in it, during the
    /// render, so every glyph a label needs is there without the host
    /// listing them first. A `file:` brush or image the style names but no
    /// source declares is read from `base_dir` when a node asks for it.
    /// Anything already bound with [`bind_source`](Self::bind_source) is
    /// kept, and a [glyph budget](Self::set_glyph_budget) applies to the
    /// stacks made here too.
    ///
    /// Only in Rust, and only with the `http` feature: it reads the file
    /// system and makes blocking requests from inside a render, which a
    /// browser cannot do and a wasm module has no host I/O for, so the JS
    /// and Go packages leave every fetch to their host. A host that does its
    /// own fetching binds instead, the same as in those shells.
    ///
    /// The banks outlive [`set_style`](Self::set_style), so a host that
    /// swaps styles calls this again for the new one's sources.
    ///
    /// Fails with [`ErrorKind::InvalidStyle`] when a source cannot be read,
    /// fetched or decoded, naming it.
    pub async fn resolve_assets(&mut self, base_dir: &Path) -> Result<(), Error> {
        self.assets.brushes_dir = Some(base_dir.to_path_buf());
        self.assets.images_dir = Some(base_dir.to_path_buf());
        ezu_paint::host::prefetch_doc_assets(&self.doc, base_dir, &mut self.assets)
            .await
            .map_err(|e| err(ErrorKind::InvalidStyle, e))?;
        // The stacks made above start uncapped; give them the budget the
        // ones `bind_source` makes get.
        self.set_glyph_budget(self.glyph_budget);
        Ok(())
    }
}
