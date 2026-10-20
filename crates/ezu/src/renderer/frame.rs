//! Rendering a print frame: every tile it covers, cropped into one page.
//!
//! [`Renderer::compute_frame`] says which tiles, at what zoom and tile
//! size; [`Renderer::render_frame`] renders them. Each tile renders from
//! the [`SourceSet`] the host returns for it, so a tile the host loaded
//! once can serve every frame tile it borders. Anything else the host
//! binds while the frame runs — the glyphs a tile turns out to need —
//! goes through a [`FrameBinder`] that borrows the renderer for one
//! tile's callback and no longer, so nothing but that binder can change
//! what the renderer draws.

use std::collections::BTreeMap;
use std::fmt;

use super::{
    BindOptions, Error, ErrorKind, Frame, FrameFeatureCollector, FrameFeatures, OutputFormat,
    PageFeatures, RenderOptions, Renderer, SourceData, SourceInfo, SourceSet, TileId,
};

/// What [`Renderer::render_frame`] hands each tile's callback: loading
/// tile data, binding into the persistent banks, and the queries a host
/// asks before it fetches.
///
/// It borrows the renderer for the length of one callback, so it cannot be
/// kept past it, and the frame holds the renderer the rest of the time —
/// the compiler is what keeps anything else from changing the renderer
/// under a frame.
pub struct FrameBinder<'a> {
    renderer: &'a mut Renderer,
}

impl FrameBinder<'_> {
    /// [`Renderer::bind_source`], for the sources bound into the renderer
    /// itself: glyphs a tile turns out to need, or a font, sprite, brush or
    /// image. A tile-scoped source is refused with
    /// [`ErrorKind::UnknownSource`]: the frame renders each tile from the
    /// set its callback returns and never reads what is bound, so tile data
    /// bound here would be dropped without a word.
    pub fn bind_source(
        &mut self,
        name: &str,
        bytes: Vec<u8>,
        opts: &BindOptions,
    ) -> Result<(), Error> {
        if let Some(decl) = self.renderer.doc.sources.get(name) {
            if SourceInfo::new(name, decl).kind.is_tile_scoped() {
                return Err(Error::new(
                    ErrorKind::UnknownSource,
                    format!(
                        "source `{name}` is tile-scoped; load it and put it in the set \
                         the tile's callback returns"
                    ),
                ));
            }
        }
        self.renderer.bind_source(name, bytes, opts)
    }

    /// [`Renderer::load_source`].
    pub fn load_source(&self, name: &str, bytes: &[u8]) -> Result<SourceData, Error> {
        self.renderer.load_source(name, bytes)
    }

    /// [`Renderer::sources`].
    pub fn sources(&self) -> Vec<SourceInfo<'_>> {
        self.renderer.sources()
    }

    /// [`Renderer::source_tile`].
    pub fn source_tile(&self, name: &str, z: u8, x: i32, y: i32) -> Result<(u8, i64, i64), Error> {
        self.renderer.source_tile(name, z, x, y)
    }

    /// [`Renderer::requested_neighbor_offsets`].
    pub fn requested_neighbor_offsets(&self, name: &str) -> Result<Vec<(i32, i32)>, Error> {
        self.renderer.requested_neighbor_offsets(name)
    }

    /// [`Renderer::needed_codepoints_in`], for the set a tile is about to
    /// render from.
    pub fn needed_codepoints_in(&self, set: &SourceSet) -> BTreeMap<&str, Vec<u32>> {
        self.renderer.needed_codepoints_in(set)
    }

    /// [`Renderer::needed_glyph_ranges_in`], for the set a tile is about to
    /// render from.
    pub fn needed_glyph_ranges_in(&self, set: &SourceSet) -> BTreeMap<&str, Vec<u32>> {
        self.renderer.needed_glyph_ranges_in(set)
    }
}

/// Why [`Renderer::render_frame`] stopped.
#[derive(Debug)]
pub enum RenderFrameError<E> {
    /// The host's callback failed for `tile`; `error` is what it returned.
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
    /// The tile data stays the host's to fetch, tile by tile, but it is
    /// handed over as a value rather than bound: before each tile
    /// `sources` is called with a [`FrameBinder`] and the tile about to
    /// render, and returns the [`SourceSet`] that tile renders from, as
    /// [`render_tile_with`](Self::render_tile_with) would — the tile
    /// itself, the neighbours
    /// [`requested_neighbor_offsets`](FrameBinder::requested_neighbor_offsets)
    /// names, the ancestor [`source_tile`](FrameBinder::source_tile)
    /// answers past a source's max-zoom. A frame's tiles border each other,
    /// so a host that keeps the [`SourceData`] it loads and puts each into
    /// the set of every tile it borders decodes each fetched tile once
    /// rather than once per tile that reads it. The binder binds what the
    /// set turns out to need into the renderer — the glyphs
    /// [`needed_codepoints_in`](FrameBinder::needed_codepoints_in) lists.
    /// An error from `sources` stops the frame and comes back as
    /// [`RenderFrameError::Bind`]. Persistent sources (fonts, sprites,
    /// brushes, images) can equally be bound once, before the call. Nothing
    /// bound with [`bind_source`](Self::bind_source) is read, and nothing is
    /// left bound by the frame.
    ///
    /// Each tile is followed by [`trim_glyphs`](Self::trim_glyphs), as a
    /// [`render_tile`](Self::render_tile) is, so a
    /// [glyph budget](Self::set_glyph_budget) holds across a frame.
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
        sources: impl FnMut(&mut FrameBinder<'_>, TileId) -> Result<SourceSet, E>,
    ) -> Result<Vec<u8>, RenderFrameError<E>> {
        self.render_frame_tiles(frame, opts, None, sources)
    }

    /// [`render_frame`](Self::render_frame), also handing back what the
    /// features nodes `features` names held over the whole page: one
    /// [`PageFeatures`] per entry, in the order given, whose
    /// [`to_geojson`](PageFeatures::to_geojson) is the file
    /// `ezu frame --features` writes for the same style and data.
    ///
    /// The values are the ones each tile's render computed on its way to
    /// the pixels, so this costs no second evaluation, and the page is the
    /// one `render_frame` returns. How a node's features over many tiles
    /// become one page — which are kept, how a key collapses copies, the
    /// order — is [`FrameFeatureCollector::finish`]'s.
    ///
    /// A name that is not a node of the style, or names one that produces
    /// something other than features, and a key that names no property,
    /// fail with [`ErrorKind::InvalidStyle`] before anything renders.
    pub fn render_frame_with_features<E>(
        &mut self,
        frame: &Frame,
        opts: RenderOptions,
        features: &FrameFeatures,
        sources: impl FnMut(&mut FrameBinder<'_>, TileId) -> Result<SourceSet, E>,
    ) -> Result<(Vec<u8>, Vec<PageFeatures>), RenderFrameError<E>> {
        let mut collector = self.frame_features(frame, features)?;
        let page = self.render_frame_tiles(frame, opts, Some(&mut collector), sources)?;
        Ok((page, collector.finish()))
    }

    fn render_frame_tiles<E>(
        &mut self,
        frame: &Frame,
        mut opts: RenderOptions,
        mut collector: Option<&mut FrameFeatureCollector>,
        mut sources: impl FnMut(&mut FrameBinder<'_>, TileId) -> Result<SourceSet, E>,
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
                let set = sources(&mut FrameBinder { renderer: self }, tile)
                    .map_err(|error| RenderFrameError::Bind { tile, error })?;
                let px = match collector.as_deref_mut() {
                    None => self.render_tile_with(frame.zoom, x, y, &set, opts.clone()),
                    Some(c) => self
                        .render_tile_with_features(frame.zoom, x, y, &set, opts.clone(), &c.nodes())
                        .map(|(px, values)| {
                            c.add(tile, &values);
                            px
                        }),
                };
                self.trim_glyphs();
                let px = px?;
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

        // All four tiles overzoom the same parent, so it is loaded once and
        // put into every tile's set.
        let parent = r.load_source("basemap", BASEMAP_Z13).expect("loads");
        let mut asked = Vec::new();
        let page = r
            .render_frame(&f, RenderOptions::default(), |_, tile| {
                asked.push(tile);
                let mut set = SourceSet::new();
                set.bind(&parent, &overzoomed())?;
                Ok::<_, Error>(set)
            })
            .expect("renders");
        assert_eq!(page.len(), 500 * 400 * 4);
        assert_eq!(
            asked,
            [
                TileId::new(14, 14554, 6454),
                TileId::new(14, 14555, 6454),
                TileId::new(14, 14554, 6455),
                TileId::new(14, 14555, 6455),
            ]
        );
        assert!(r.bound_sources().is_empty(), "the frame binds no tile data");

        // The same page put together by hand, pixel by pixel, from tiles
        // bound and rendered one at a time.
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

    /// The page features of a frame are the page and the features its
    /// tiles' renders computed, assembled as a collector fed by hand
    /// assembles them.
    #[test]
    fn a_frame_hands_back_its_page_features() {
        use crate::renderer::{FeatureCoords, FrameFeatureNode};
        let mut r = Renderer::new(STAINED_GLASS).expect("builds");
        let f = corner(&r);
        let parent = r.load_source("basemap", BASEMAP_Z13).expect("loads");
        let set = |_: &mut FrameBinder<'_>, _: TileId| {
            let mut set = SourceSet::new();
            set.bind(&parent, &overzoomed())?;
            Ok::<_, Error>(set)
        };
        let req = FrameFeatures {
            nodes: vec![
                FrameFeatureNode::new("roads_f"),
                FrameFeatureNode::keyed("water_f", ["kind"]),
            ],
            coords: FeatureCoords::Px,
        };
        let plain = r
            .render_frame(&f, RenderOptions::default(), set)
            .expect("renders");
        let (page, features) = r
            .render_frame_with_features(&f, RenderOptions::default(), &req, set)
            .expect("renders");
        assert!(page == plain, "the page is render_frame's");
        assert_eq!(
            features.iter().map(PageFeatures::node).collect::<Vec<_>>(),
            ["roads_f", "water_f"]
        );
        assert!(!features[0].is_empty());

        let mut by_hand = r.frame_features(&f, &req).expect("a collector");
        let opts = RenderOptions {
            format: OutputFormat::Rgba,
            tile_size: Some(f.tile_size),
            ..RenderOptions::default()
        };
        // Bottom row first: the order tiles are added in does not matter.
        for y in f.tiles_y.clone().rev() {
            for x in f.tiles_x.clone() {
                let mut set = SourceSet::new();
                set.bind(&parent, &overzoomed()).expect("binds");
                let (_, values) = r
                    .render_tile_with_features(f.zoom, x, y, &set, opts.clone(), &by_hand.nodes())
                    .expect("renders");
                by_hand.add(TileId::new(f.zoom, x, y), &values);
            }
        }
        let want: Vec<String> = by_hand.finish().iter().map(|p| p.to_geojson()).collect();
        let got: Vec<String> = features.iter().map(|p| p.to_geojson()).collect();
        assert_eq!(got, want);

        for bad in [
            FrameFeatureNode::new("nope"),
            FrameFeatureNode::new("out"),
            FrameFeatureNode::keyed("roads_f", Vec::<String>::new()),
        ] {
            let req = FrameFeatures {
                nodes: vec![bad],
                coords: FeatureCoords::LonLat,
            };
            let e = r
                .render_frame_with_features(&f, RenderOptions::default(), &req, |_, _| {
                    panic!("nothing renders")
                })
                .map(|_| ())
                .unwrap_err();
            assert!(
                matches!(&e, RenderFrameError::<Error>::Render(e) if e.kind() == ErrorKind::InvalidStyle),
                "{e}"
            );
        }
    }

    #[test]
    fn a_frame_stops_at_a_failed_bind_and_refuses_what_contradicts_it() {
        let mut r = Renderer::new(STAINED_GLASS).expect("builds");
        let f = corner(&r);

        let mut calls = 0;
        let e = r
            .render_frame(&f, RenderOptions::default(), |b, tile| {
                calls += 1;
                let parent = b.load_source("basemap", BASEMAP_Z13).expect("loads");
                let mut set = SourceSet::new();
                set.bind(&parent, &overzoomed()).expect("binds");
                if tile.x == 14555 {
                    return Err("fetch failed");
                }
                Ok(set)
            })
            .unwrap_err();
        assert!(
            matches!(e, RenderFrameError::Bind { tile, error: "fetch failed" } if tile == TileId::new(14, 14555, 6454))
        );
        assert_eq!(e.to_string(), "binding tile 14/14555/6454: fetch failed");
        assert_eq!(calls, 2);

        // Tile data bound through the binder would never be read, so it is
        // refused rather than dropped.
        let e = r
            .render_frame(&f, RenderOptions::default(), |b, _| {
                b.bind_source("basemap", BASEMAP_Z13.to_vec(), &overzoomed())?;
                Ok::<_, Error>(SourceSet::new())
            })
            .unwrap_err();
        assert!(
            matches!(&e, RenderFrameError::Bind { error, .. } if error.kind() == ErrorKind::UnknownSource),
            "{e}"
        );
        assert!(
            r.bound_sources().is_empty(),
            "the refused bind left nothing"
        );

        let no_bind = |_: &mut FrameBinder<'_>, _: TileId| Ok::<_, ()>(SourceSet::new());
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
