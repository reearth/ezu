//! Rendering a print frame: every tile it covers, cropped into one page.
//!
//! [`Renderer::compute_frame`] says which tiles, at what zoom and tile
//! size; [`Renderer::render_frame`] renders them. Binding stays the
//! host's, tile by tile, through a [`FrameBinder`] that borrows the
//! renderer for one tile's bind and no longer — so while a frame renders,
//! nothing but that binder can change what the renderer draws.

use std::collections::BTreeMap;
use std::fmt;

use super::{
    BindOptions, Error, ErrorKind, Frame, OutputFormat, RenderOptions, Renderer, SourceInfo, TileId,
};

/// What [`Renderer::render_frame`] hands each tile's bind: `bind_source`
/// for that tile, and the queries a bind asks before it fetches.
///
/// It borrows the renderer for the length of one bind, so it cannot be
/// kept past it, and the frame holds the renderer the rest of the time —
/// the compiler is what keeps anything else from binding into a frame's
/// tiles.
pub struct FrameBinder<'a> {
    renderer: &'a mut Renderer,
}

impl FrameBinder<'_> {
    /// [`Renderer::bind_source`], for the tile this binder was handed with.
    pub fn bind_source(
        &mut self,
        name: &str,
        bytes: Vec<u8>,
        opts: &BindOptions,
    ) -> Result<(), Error> {
        self.renderer.bind_source(name, bytes, opts)
    }

    /// [`Renderer::sources`].
    pub fn sources(&self) -> Vec<SourceInfo<'_>> {
        self.renderer.sources()
    }

    /// [`Renderer::bound_sources`].
    pub fn bound_sources(&self) -> Vec<String> {
        self.renderer.bound_sources()
    }

    /// [`Renderer::source_tile`].
    pub fn source_tile(&self, name: &str, z: u8, x: i32, y: i32) -> Result<(u8, i64, i64), Error> {
        self.renderer.source_tile(name, z, x, y)
    }

    /// [`Renderer::requested_neighbor_offsets`].
    pub fn requested_neighbor_offsets(&self, name: &str) -> Result<Vec<(i32, i32)>, Error> {
        self.renderer.requested_neighbor_offsets(name)
    }

    /// [`Renderer::needed_codepoints`], for what this tile has bound so
    /// far.
    pub fn needed_codepoints(&self) -> Result<BTreeMap<&str, Vec<u32>>, Error> {
        self.renderer.needed_codepoints()
    }

    /// [`Renderer::needed_glyph_ranges`], for what this tile has bound so
    /// far.
    pub fn needed_glyph_ranges(&self) -> Result<BTreeMap<&str, Vec<u32>>, Error> {
        self.renderer.needed_glyph_ranges()
    }
}

/// Why [`Renderer::render_frame`] stopped.
#[derive(Debug)]
pub enum RenderFrameError<E> {
    /// The host's bind failed for `tile`; `error` is what it returned.
    Bind { tile: TileId, error: E },
    /// The frame could not be rendered: it has no size, it asks for a tile
    /// size other than its own, or a tile failed to render.
    Render(Error),
}

impl<E: fmt::Display> fmt::Display for RenderFrameError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderFrameError::Bind { tile, error } => {
                write!(f, "binding tile {}/{}/{}: {error}", tile.z, tile.x, tile.y)
            }
            RenderFrameError::Render(e) => e.fmt(f),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for RenderFrameError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RenderFrameError::Bind { error, .. } => Some(error),
            RenderFrameError::Render(e) => Some(e),
        }
    }
}

impl<E> From<Error> for RenderFrameError<E> {
    fn from(e: Error) -> Self {
        RenderFrameError::Render(e)
    }
}

impl Renderer {
    /// Render every tile `frame` covers and copy each one's share into one
    /// frame-sized page — the same tiles, at the same tile size, cropped
    /// the same way as `ezu frame`. The page is straight
    /// (un-premultiplied) RGBA, `width × height × 4` bytes, as
    /// [`OutputFormat::Rgba`] is.
    ///
    /// Binding stays the host's, tile by tile, as for
    /// [`render_tile`](Self::render_tile). Before each tile the
    /// tile-scoped sources are cleared and `bind` is called with a
    /// [`FrameBinder`] and the tile about to render; it binds what that
    /// tile needs — the tile itself, the neighbours
    /// [`requested_neighbor_offsets`](FrameBinder::requested_neighbor_offsets)
    /// names, the ancestor [`source_tile`](FrameBinder::source_tile)
    /// answers past a source's max-zoom, the glyphs
    /// [`needed_codepoints`](FrameBinder::needed_codepoints) lists. An
    /// error from `bind` stops the frame and comes back as
    /// [`RenderFrameError::Bind`]. Persistent sources (fonts, sprites,
    /// brushes, images) are bound once, before the call, as usual. However
    /// the frame ends, nothing of its tiles is left bound.
    ///
    /// The tiles render at `frame.tile_size` per render, so the renderer's
    /// own tile size is left as it was. `opts` carries the rest of a render
    /// — `pad`, `params`; its `format` is replaced by raw pixels, and a
    /// `tile_size` other than the frame's is refused with
    /// [`ErrorKind::InvalidStyle`], as is a frame with no size.
    ///
    /// Tiles render one after another, row by row from the top.
    pub fn render_frame<E>(
        &mut self,
        frame: &Frame,
        opts: RenderOptions,
        mut bind: impl FnMut(&mut FrameBinder<'_>, TileId) -> Result<(), E>,
    ) -> Result<Vec<u8>, RenderFrameError<E>> {
        let result = self.render_frame_tiles(frame, opts, &mut bind);
        self.clear_sources();
        result
    }

    fn render_frame_tiles<E>(
        &mut self,
        frame: &Frame,
        mut opts: RenderOptions,
        bind: &mut impl FnMut(&mut FrameBinder<'_>, TileId) -> Result<(), E>,
    ) -> Result<Vec<u8>, RenderFrameError<E>> {
        let invalid = |msg: String| Error::new(ErrorKind::InvalidStyle, msg);
        if frame.tile_size == 0 || frame.width == 0 || frame.height == 0 {
            return Err(
                invalid("the frame has no size; compute it with compute_frame".into()).into(),
            );
        }
        if let Some(t) = opts.tile_size.filter(|t| *t != frame.tile_size) {
            return Err(invalid(format!(
                "the frame renders at tile size {}; leave tile_size unset, not {t}",
                frame.tile_size
            ))
            .into());
        }
        opts.format = OutputFormat::Rgba;
        opts.tile_size = Some(frame.tile_size);

        let t = frame.tile_size as usize;
        let mut page = vec![0u8; frame.width as usize * frame.height as usize * 4];
        for y in frame.tiles_y.clone() {
            for x in frame.tiles_x.clone() {
                let tile = TileId::new(frame.zoom, x, y);
                self.clear_sources();
                bind(&mut FrameBinder { renderer: self }, tile)
                    .map_err(|error| RenderFrameError::Bind { tile, error })?;
                let px = self.render_tile(frame.zoom, x, y, opts.clone())?;
                if px.len() != t * t * 4 {
                    return Err(Error::new(
                        ErrorKind::RenderFailed,
                        format!(
                            "tile {}/{x}/{y} came back as {} bytes, want {} for {t}x{t}",
                            frame.zoom,
                            px.len(),
                            t * t * 4
                        ),
                    )
                    .into());
                }
                for row in frame.tile_rows(x, y) {
                    page[row.dst..row.dst + row.len]
                        .copy_from_slice(&px[row.src..row.src + row.len]);
                }
            }
        }
        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::FrameRequest;

    const STAINED_GLASS: &str = include_str!("../../examples/styles/stained-glass.json");
    const BASEMAP_Z13: &[u8] = include_bytes!("../../../../go/testdata/basemap-13-7277-3227.mvt");

    /// A 500 × 400 px frame on the corner where four z14 tiles meet, so
    /// every tile is cropped on two sides, at a tile size (391) that is
    /// nobody's default. Only their z13 parent is in the test data, so it
    /// is overzoomed. The Go and JS tests pin this page's pixels to the
    /// CLI's.
    fn corner(r: &Renderer) -> Frame {
        let f = r
            .compute_frame(&FrameRequest {
                center_lon: 139.812,
                center_lat: 35.6216,
                scale: 40000.0,
                dpi: 200.0,
                width: 500,
                height: 400,
                zoom: Some(14),
                style_tile_size: r.tile_size(),
            })
            .expect("a frame");
        assert_eq!(
            (f.tile_size, f.tiles_x.clone(), f.tiles_y.clone()),
            (391, 14554..14556, 6454..6456)
        );
        f
    }

    fn overzoomed() -> BindOptions {
        BindOptions {
            source_zoom: Some(13),
            ..BindOptions::default()
        }
    }

    #[test]
    fn a_frame_is_its_tiles_cropped_into_one_page() {
        let mut r = Renderer::new(STAINED_GLASS).expect("builds");
        let f = corner(&r);

        let mut bound = Vec::new();
        let page = r
            .render_frame(&f, RenderOptions::default(), |b, tile| {
                assert!(
                    b.bound_sources().is_empty(),
                    "the previous tile was cleared"
                );
                bound.push(tile);
                b.bind_source("basemap", BASEMAP_Z13.to_vec(), &overzoomed())
            })
            .expect("renders");
        assert_eq!(page.len(), 500 * 400 * 4);
        assert_eq!(
            bound,
            [
                TileId::new(14, 14554, 6454),
                TileId::new(14, 14555, 6454),
                TileId::new(14, 14554, 6455),
                TileId::new(14, 14555, 6455),
            ]
        );
        assert!(
            r.bound_sources().is_empty(),
            "nothing of the last tile is left bound"
        );

        // The same page put together by hand, pixel by pixel.
        let t = f.tile_size as i64;
        let mut want = vec![0u8; page.len()];
        for y in f.tiles_y.clone() {
            for x in f.tiles_x.clone() {
                r.clear_sources();
                r.bind_source("basemap", BASEMAP_Z13.to_vec(), &overzoomed())
                    .expect("binds");
                let opts = RenderOptions {
                    format: OutputFormat::Rgba,
                    tile_size: Some(f.tile_size),
                    ..RenderOptions::default()
                };
                let px = r.render_tile(f.zoom, x, y, opts).expect("renders");
                let (dx, dy) = f.tile_offset(x, y);
                for py in dy.max(0)..(dy + t).min(f.height as i64) {
                    for qx in dx.max(0)..(dx + t).min(f.width as i64) {
                        let src = (((py - dy) * t + (qx - dx)) * 4) as usize;
                        let dst = ((py * f.width as i64 + qx) * 4) as usize;
                        want[dst..dst + 4].copy_from_slice(&px[src..src + 4]);
                    }
                }
            }
        }
        r.clear_sources();
        assert!(page == want, "render_frame differs from its tiles");

        // The renderer's own tile size is untouched.
        assert_eq!(r.render_tile_size(), 512);
    }

    #[test]
    fn a_frame_stops_at_a_failed_bind_and_refuses_what_contradicts_it() {
        let mut r = Renderer::new(STAINED_GLASS).expect("builds");
        let f = corner(&r);

        let mut calls = 0;
        let e = r
            .render_frame(&f, RenderOptions::default(), |b, tile| {
                calls += 1;
                b.bind_source("basemap", BASEMAP_Z13.to_vec(), &overzoomed())
                    .expect("binds");
                if tile.x == 14555 {
                    return Err("fetch failed");
                }
                Ok(())
            })
            .unwrap_err();
        assert!(
            matches!(e, RenderFrameError::Bind { tile, error: "fetch failed" } if tile == TileId::new(14, 14555, 6454))
        );
        assert_eq!(e.to_string(), "binding tile 14/14555/6454: fetch failed");
        assert_eq!(calls, 2);
        assert!(
            r.bound_sources().is_empty(),
            "a failed frame leaves nothing bound"
        );

        let no_bind = |_: &mut FrameBinder<'_>, _: TileId| Ok::<(), ()>(());
        let other_size = RenderOptions {
            tile_size: Some(256),
            ..RenderOptions::default()
        };
        let refused = |e: RenderFrameError<()>| matches!(e, RenderFrameError::Render(e) if e.kind() == ErrorKind::InvalidStyle);
        assert!(refused(
            r.render_frame(&f, other_size, no_bind).unwrap_err()
        ));
        let sizeless = Frame {
            tile_size: 0,
            ..f.clone()
        };
        assert!(refused(
            r.render_frame(&sizeless, RenderOptions::default(), no_bind)
                .unwrap_err()
        ));
    }
}
