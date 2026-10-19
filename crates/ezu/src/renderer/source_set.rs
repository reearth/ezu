//! Decoded tile payloads and the sets a render reads them from.
//!
//! [`Renderer::bind_source`](super::Renderer::bind_source) keeps one
//! binding table on the renderer: bind, render, clear, bind the next tile.
//! That is state every host has to reset by hand, it rules out two renders
//! at once on one renderer, and it decodes a payload each time it is bound,
//! which for a vector tile that is also its neighbours' neighbour is up to
//! nine times over a block of tiles.
//!
//! The two types here take the table apart. A [`SourceData`] is one
//! payload, decoded once by [`Renderer::load_source`](super::Renderer::load_source)
//! and immutable after that. A [`SourceSet`] says which payloads one render
//! reads, and where in the 3×3 neighbourhood each sits;
//! [`Renderer::render_tile_with`](super::Renderer::render_tile_with) reads
//! that set and nothing else that is tile-scoped. Both clone in constant
//! time, so a host loads each tile it fetched once, puts it into the set of
//! every tile it borders, and drops it when no set needs it any more.

use std::collections::HashMap;
use std::sync::Arc;

use ezu_features::FeatureLayer;
use ezu_paint::host::{DemTile, RasterTile};
use ezu_style::DemEncoding;

use super::{already_bound_otherwise, BindOptions, Error};

/// One tile-scoped source's payload, decoded and ready to bind to any
/// number of renders.
///
/// Made by [`Renderer::load_source`](super::Renderer::load_source), which
/// decodes the bytes there and then, so a payload that will not decode
/// fails while the host still knows which fetch produced it, and a payload
/// that does is never decoded again. What it holds depends on the source's
/// kind: an MVT tile's layers, a DEM tile's elevations, an imagery tile's
/// pixels, or a parsed GeoJSON document. None of these depend on which tile
/// is being drawn; the per-tile work — overzoom resampling, the 3×3 stitch,
/// projecting GeoJSON into a tile's frame — happens in each render.
///
/// It remembers the source it was loaded for, and a render checks that the
/// active style still declares that source the same way, so data loaded
/// under one style is refused, not misread, after
/// [`set_style`](super::Renderer::set_style) swaps it.
#[derive(Clone)]
pub struct SourceData {
    pub(super) name: Arc<str>,
    pub(super) payload: Payload,
}

#[derive(Clone)]
pub(super) enum Payload {
    Mvt(Arc<[Arc<FeatureLayer>]>),
    /// Elevations, with the encoding they were decoded with: the same
    /// bytes read as another encoding are different heights.
    Dem(Arc<DemTile>, DemEncoding),
    Raster(Arc<RasterTile>),
    GeoJson(Arc<serde_json::Value>),
    /// DEM or imagery bytes bound with `bind_source`, which has always
    /// decoded these at render time — once the tile is known, so a decode
    /// error can name it — and keeps doing so.
    DemBytes(Arc<[u8]>),
    RasterBytes(Arc<[u8]>),
}

/// What a payload is, for telling a host it put two kinds under one name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Vector,
    Dem,
    Raster,
    GeoJson,
}

impl Payload {
    fn family(&self) -> Family {
        match self {
            Payload::Mvt(_) => Family::Vector,
            Payload::Dem(..) | Payload::DemBytes(_) => Family::Dem,
            Payload::Raster(_) | Payload::RasterBytes(_) => Family::Raster,
            Payload::GeoJson(_) => Family::GeoJson,
        }
    }
}

impl SourceData {
    /// The `sources.<name>` entry this payload was loaded for.
    pub fn source(&self) -> &str {
        &self.name
    }
}

impl std::fmt::Debug for SourceData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match &self.payload {
            Payload::Mvt(_) => "mvt",
            Payload::Dem(..) | Payload::DemBytes(_) => "dem",
            Payload::Raster(_) | Payload::RasterBytes(_) => "raster",
            Payload::GeoJson(_) => "geojson",
        };
        f.debug_struct("SourceData")
            .field("source", &self.name)
            .field("kind", &kind)
            .finish()
    }
}

/// One payload's place in a render: the data, and — when it is an
/// ancestor's — the zoom it was encoded at.
#[derive(Clone)]
pub(super) struct Placed {
    pub(super) data: SourceData,
    pub(super) source_zoom: Option<u8>,
}

/// Every tile-scoped payload one render reads, keyed by source and by
/// neighbour offset.
///
/// It is the binding table `bind_source` fills, as a value: build one per
/// tile with [`bind`](Self::bind) and hand it to
/// [`Renderer::render_tile_with`](super::Renderer::render_tile_with), which
/// only reads it. A set says nothing about the tile it is for — offsets are
/// relative — so nothing stops the same set rendering two tiles; it is the
/// host's to pair each tile with its neighbourhood.
///
/// Brushes, images, sprites, fonts and glyphs are not here. They belong to
/// the renderer, bound once with `bind_source` and shared by every render.
#[derive(Clone, Default)]
pub struct SourceSet {
    pub(super) entries: HashMap<Arc<str>, HashMap<(i32, i32), Placed>>,
}

impl SourceSet {
    /// An empty set: a render from it draws what the style draws with no
    /// tile data at all.
    pub fn new() -> Self {
        Self::default()
    }

    /// Put `data` at [`BindOptions::coord`] for its source, replacing what
    /// was there. [`BindOptions::source_zoom`] says the data is an
    /// ancestor's and has to be resampled or reprojected into the tile,
    /// exactly as for `bind_source`; [`BindOptions::index`] means nothing
    /// to a tile-scoped source and is ignored.
    ///
    /// Fails with [`ErrorKind::UnknownSource`](super::ErrorKind::UnknownSource)
    /// when the source already holds data of another kind.
    pub fn bind(&mut self, data: &SourceData, opts: &BindOptions) -> Result<&mut Self, Error> {
        let slots = self.entries.entry(data.name.clone()).or_default();
        if slots
            .values()
            .any(|p| p.data.payload.family() != data.payload.family())
        {
            return Err(already_bound_otherwise(&data.name));
        }
        slots.insert(
            opts.coord,
            Placed {
                data: data.clone(),
                source_zoom: opts.source_zoom,
            },
        );
        Ok(self)
    }

    /// Whether anything is bound for `source`.
    pub fn contains(&self, source: &str) -> bool {
        self.entries.contains_key(source)
    }

    /// Whether nothing is bound at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn get(&self, source: &str) -> Option<&HashMap<(i32, i32), Placed>> {
        self.entries.get(source)
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
}

impl std::fmt::Debug for SourceSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut m = f.debug_map();
        for (name, slots) in &self.entries {
            let mut offsets: Vec<_> = slots.keys().copied().collect();
            offsets.sort_unstable();
            m.entry(name, &offsets);
        }
        m.finish()
    }
}
