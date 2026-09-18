//! What a style declares, for a host that has to go and fetch it.
//!
//! [`Renderer::bound_sources`](crate::Renderer::bound_sources) answers what
//! is already bound; this answers the question that comes first — what the
//! style asks for, of which kind, and where from. A host driving the bind
//! loop needs that before it can bind anything, and the alternative is for
//! every host to parse the style a second time and re-derive the dispatch
//! `bind_source` already performs.

use std::borrow::Cow;

use ezu_style::{SourceDecl, SpriteIndex};

/// The kind a style declares a source as: the `type` field, and what
/// [`bind_source`](crate::Renderer::bind_source) dispatches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// Vector tiles from an XYZ template or a TileJSON document.
    Mvt,
    /// Vector tiles from a PMTiles archive. Bound exactly like [`Mvt`](Self::Mvt).
    Pmtiles,
    /// Elevation tiles, decoded and 3×3 stitched at render time.
    Dem,
    /// RGBA imagery tiles, decoded and 3×3 stitched at render time.
    Raster,
    /// A GeoJSON document, projected per tile. Inline `data` carries its own
    /// payload and needs no binding — such a source reports no
    /// [`url`](SourceInfo::url).
    GeoJson,
    /// A sprite sheet: an atlas image plus its index.
    Sprite,
    /// An outline font file (TTF / OTF / TTC).
    Font,
    /// A MapLibre glyph endpoint, bound one SDF glyph PBF at a time.
    Glyphs,
    /// A `.myb` brush definition.
    Brush,
    /// A standalone PNG / WebP image.
    Image,
}

impl SourceKind {
    /// The name the style spells this kind with, e.g. `"mvt"`, `"geojson"`.
    pub const fn name(self) -> &'static str {
        match self {
            SourceKind::Mvt => "mvt",
            SourceKind::Pmtiles => "pmtiles",
            SourceKind::Dem => "dem",
            SourceKind::Raster => "raster",
            SourceKind::GeoJson => "geojson",
            SourceKind::Sprite => "sprite",
            SourceKind::Font => "font",
            SourceKind::Glyphs => "glyphs",
            SourceKind::Brush => "brush",
            SourceKind::Image => "image",
        }
    }

    /// Whether a binding of this kind belongs to one tile, and so is dropped
    /// by [`clear_sources`](crate::Renderer::clear_sources) and rebound for
    /// the next tile. The rest go into the persistent banks and are bound
    /// once for the life of the renderer.
    pub const fn is_tile_scoped(self) -> bool {
        matches!(
            self,
            SourceKind::Mvt
                | SourceKind::Pmtiles
                | SourceKind::Dem
                | SourceKind::Raster
                | SourceKind::GeoJson
        )
    }
}

/// One entry of the style's `sources` block, as much of it as a host needs
/// to fetch the bytes and bind them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo<'a> {
    /// The `sources.<name>` key, which is what `bind_source` takes.
    pub name: &'a str,
    /// What the style declared it as.
    pub kind: SourceKind,
    /// Where the bytes come from, as the style wrote it: an XYZ template
    /// with `{z}/{x}/{y}` (or a TileJSON URL) for a tile pyramid, a
    /// PMTiles archive, a document URL, a file `src`, or — for `glyphs` —
    /// the endpoint template with `{fontstack}` already substituted and
    /// `{range}` left for the host to fill in per block.
    ///
    /// `None` only for a `geojson` source with inline `data`: the style
    /// carries the payload, so there is nothing to fetch and nothing to
    /// bind.
    pub url: Option<Cow<'a, str>>,
    /// A `sprite` source's index document, when the style gives a URL for
    /// it rather than inlining it. Fetch it and pass the text as
    /// [`BindOptions::index`](crate::BindOptions::index) alongside the
    /// atlas image. `None` for every other kind, and for an inline index.
    pub index_url: Option<&'a str>,
}

impl<'a> SourceInfo<'a> {
    pub(crate) fn new(name: &'a str, decl: &'a SourceDecl) -> Self {
        let (kind, url, index_url) = match decl {
            SourceDecl::Mvt(s) => (SourceKind::Mvt, Some(borrowed(&s.url)), None),
            SourceDecl::Pmtiles(s) => (SourceKind::Pmtiles, Some(borrowed(&s.url)), None),
            SourceDecl::Dem(s) => (SourceKind::Dem, Some(borrowed(&s.url)), None),
            SourceDecl::Raster(s) => (SourceKind::Raster, Some(borrowed(&s.url)), None),
            SourceDecl::GeoJson(s) => (
                SourceKind::GeoJson,
                s.url.as_deref().map(Cow::Borrowed),
                None,
            ),
            SourceDecl::Sprite(s) => (
                SourceKind::Sprite,
                Some(borrowed(&s.image)),
                match &s.index {
                    SpriteIndex::Url(url) => Some(url.as_str()),
                    SpriteIndex::Inline(_) => None,
                },
            ),
            SourceDecl::Font(s) => (SourceKind::Font, Some(borrowed(&s.url)), None),
            // `{fontstack}` is resolved here rather than left to the host:
            // it is percent-encoded the way MapLibre encodes it, and a host
            // doing that itself is the style interpretation this call
            // exists to remove.
            SourceDecl::Glyphs(s) => (SourceKind::Glyphs, Some(Cow::Owned(s.asset_key())), None),
            SourceDecl::Brush(s) => (SourceKind::Brush, Some(borrowed(&s.src)), None),
            SourceDecl::Image(s) => (SourceKind::Image, Some(borrowed(&s.src)), None),
        };
        Self {
            name,
            kind,
            url,
            index_url,
        }
    }
}

fn borrowed(s: &str) -> Cow<'_, str> {
    Cow::Borrowed(s)
}
