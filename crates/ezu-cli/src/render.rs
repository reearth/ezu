//! Rendering through `ezu::renderer`, the way any host that does its own
//! fetching renders.
//!
//! For each tile, the bytes of every source tile its render reads are
//! fetched: the tile itself, the neighbours the style asks for, and an
//! ancestor where a source stops at its `max-zoom` or a tile is missing.
//! Each is decoded once into a `SourceData` that is kept for the renders
//! around it — the centre of one tile is a neighbour of the next eight, and
//! past a source's `max-zoom` a whole page reads the same ancestor — and the
//! tile renders from a `SourceSet` of them. One `Renderer` serves every
//! render in flight, so the tiles of a page or a pyramid render side by
//! side.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use bytes::Bytes;
use ezu::core::TileId;
use ezu::renderer::{BindOptions, Error, RenderOptions, Renderer, SourceData, SourceSet};
use ezu::style::{Document, OnMissing, SourceDecl};

use crate::source::{FeatureSources, PyramidSources, TileByteCache};

pub(crate) type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// One source tile a render reads, fetched.
struct Fetched {
    source: Arc<str>,
    /// The tile the bytes belong to: the one at `coord`, or an ancestor.
    from: TileId,
    bytes: Bytes,
    /// Where in the rendered tile's 3×3 neighbourhood the bytes go.
    coord: (i32, i32),
}

/// What [`TileRenderer::open`] needs besides the style.
pub(crate) struct Open<'a> {
    /// The style as written; the renderer parses it itself.
    pub style: &'a str,
    pub doc: &'a Document,
    /// What the style's graph reads: a `dem` or `raster` source nothing
    /// reads is not fetched.
    pub requested: BTreeSet<String>,
    /// The canvas margin every render is drawn with.
    pub pad: u32,
    /// Where `file:` paths in the style resolve.
    pub assets_dir: &'a Path,
    /// The style's vector tile sources, opened by the caller, which may
    /// share them between styles.
    pub vector: Arc<FeatureSources>,
    pub overzoom_levels: u8,
    /// `--param` overrides, as `(name, value)` text.
    pub params: Vec<(String, String)>,
    /// Where fetched tile bytes are kept, and whether only remote ones are.
    pub byte_cache: Option<(&'a Arc<TileByteCache>, bool)>,
    /// How many bytes of source tiles may stay decoded.
    pub decoded_budget: usize,
    /// The render cache's pixel budget, when not the renderer's default.
    pub cache_budget: Option<usize>,
}

/// A style ready to render tiles from its sources.
pub(crate) struct TileRenderer {
    renderer: Arc<Renderer>,
    vector: Arc<FeatureSources>,
    pyramids: Arc<PyramidSources>,
    /// Remote GeoJSON documents, loaded once: one document covers every
    /// tile, which projects it into its own frame.
    documents: Arc<Vec<SourceData>>,
    decoded: Arc<DecodedCache>,
    requested: Arc<BTreeSet<String>>,
    overzoom_levels: u8,
    /// What every render is drawn with: the margin, the params, and
    /// whether the graph evaluates in parallel.
    base: RenderOptions,
}

impl TileRenderer {
    /// Build the renderer, let it read the style's assets, open the `dem`
    /// and `raster` sources and load the remote GeoJSON documents.
    pub(crate) async fn open(o: Open<'_>) -> Result<Self, Box<dyn std::error::Error>> {
        let mut renderer = Renderer::new(o.style)?;
        renderer.resolve_assets(o.assets_dir).await?;
        if o.cache_budget.is_some() {
            renderer.set_cache_budget(o.cache_budget);
        }
        let mut pyramids = PyramidSources::open(o.doc, o.assets_dir).await?;
        if let Some((cache, remote_only)) = o.byte_cache {
            pyramids = pyramids.with_byte_cache(cache, remote_only);
        }
        let documents = load_documents(&renderer, o.doc, o.assets_dir).await?;
        Ok(Self {
            renderer: Arc::new(renderer),
            vector: o.vector,
            pyramids: Arc::new(pyramids),
            documents: Arc::new(documents),
            decoded: Arc::new(DecodedCache::new(o.decoded_budget)),
            requested: Arc::new(o.requested),
            overzoom_levels: o.overzoom_levels,
            base: RenderOptions {
                pad: Some(o.pad),
                params: o.params,
                parallel: !serial_eval(),
                ..RenderOptions::default()
            },
        })
    }

    /// Render every tile at `size` px, starting a fresh render cache when
    /// that is not the size it renders at already. Only between renders:
    /// none may be in flight.
    pub(crate) fn set_tile_size(&mut self, size: u32) -> Result<(), Error> {
        Arc::get_mut(&mut self.renderer)
            .expect("no render holds the renderer between pages")
            .set_tile_size(Some(size))
    }

    /// Render `tile`: fetch what it reads, decode what is not decoded yet,
    /// and hand the renderer, the tile's set and the render options to
    /// `draw`, on a blocking thread.
    pub(crate) async fn render<T: Send + 'static>(
        &self,
        tile: TileId,
        draw: impl FnOnce(&Renderer, &SourceSet, RenderOptions) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, BoxError> {
        let fetched = self.fetch(tile).await?;
        let renderer = Arc::clone(&self.renderer);
        let decoded = Arc::clone(&self.decoded);
        let documents = Arc::clone(&self.documents);
        let opts = self.base.clone();
        tokio::task::spawn_blocking(move || -> Result<T, BoxError> {
            let mut set = SourceSet::new();
            for f in fetched {
                let data = decoded.load(&renderer, &f.source, f.from, &f.bytes)?;
                let bind = BindOptions {
                    coord: f.coord,
                    source_zoom: (f.from.z < tile.z).then_some(f.from.z),
                    ..BindOptions::default()
                };
                set.bind(&data, &bind)?;
            }
            for doc in documents.iter() {
                set.bind(doc, &BindOptions::default())?;
                for coord in renderer.requested_neighbor_offsets(doc.source())? {
                    let bind = BindOptions {
                        coord,
                        ..BindOptions::default()
                    };
                    set.bind(doc, &bind)?;
                }
            }
            Ok(draw(&renderer, &set, opts)?)
        })
        .await?
    }

    /// Every source tile `tile`'s render reads: each vector source's tile
    /// and the neighbours the graph asks for, falling back to an ancestor
    /// on a miss; and each `dem` and `raster` source the graph reads, with
    /// the 3×3 its `neighbor-fetch` asks for.
    async fn fetch(&self, tile: TileId) -> Result<Vec<Fetched>, BoxError> {
        let vector = futures::future::try_join_all(self.vector.iter().map(|s| async move {
            let offsets = self.renderer.requested_neighbor_offsets(&s.name)?;
            let mut got = Vec::new();
            if let Some((bytes, from)) = s.fetch(tile, self.overzoom_levels).await? {
                got.push(Fetched {
                    source: Arc::clone(&s.name),
                    from,
                    bytes,
                    coord: (0, 0),
                });
            }
            if !offsets.is_empty() {
                for (coord, (bytes, from)) in s
                    .fetch_neighbors(tile, &offsets, self.overzoom_levels)
                    .await?
                {
                    got.push(Fetched {
                        source: Arc::clone(&s.name),
                        from,
                        bytes,
                        coord,
                    });
                }
            }
            Ok::<_, BoxError>(got)
        }));
        let pyramids = futures::future::try_join_all(
            self.pyramids
                .iter()
                .filter(|p| self.requested.contains(&*p.name))
                .map(|p| async move {
                    let Some((bytes, from)) = p.fetch(tile).await? else {
                        // A missing centre leaves the source out: the node
                        // reading it draws nothing, unless the style asked
                        // for an error instead.
                        if p.on_missing() == OnMissing::Error {
                            return Err(format!(
                                "source `{}`: tile {}/{}/{} is missing (on-missing: error)",
                                p.name, tile.z, tile.x, tile.y
                            )
                            .into());
                        }
                        return Ok(Vec::new());
                    };
                    let mut got = vec![Fetched {
                        source: Arc::clone(&p.name),
                        from,
                        bytes,
                        coord: (0, 0),
                    }];
                    let world = 1i64 << tile.z;
                    let neighbours = self
                        .renderer
                        .requested_neighbor_offsets(&p.name)?
                        .into_iter()
                        .filter_map(|(dx, dy)| {
                            let y = tile.y as i64 + dy as i64;
                            // Off the top or bottom of the world there is
                            // no tile; the stitch clamps that edge.
                            (0..world).contains(&y).then(|| {
                                let x = (tile.x as i64 + dx as i64).rem_euclid(world);
                                ((dx, dy), TileId::new(tile.z, x as u32, y as u32))
                            })
                        });
                    let fetched =
                        futures::future::try_join_all(neighbours.map(|(coord, at)| async move {
                            Ok::<_, BoxError>(p.fetch(at).await?.map(|hit| (coord, hit)))
                        }))
                        .await?;
                    for (coord, (bytes, from)) in fetched.into_iter().flatten() {
                        got.push(Fetched {
                            source: Arc::clone(&p.name),
                            from,
                            bytes,
                            coord,
                        });
                    }
                    Ok::<_, BoxError>(got)
                }),
        );
        let (vector, pyramids) = futures::future::try_join(vector, pyramids).await?;
        Ok(vector.into_iter().chain(pyramids).flatten().collect())
    }
}

/// Load every `geojson` source the style reads from a URL rather than
/// carrying inline. A string `data` is MapLibre's other spelling of `url`.
async fn load_documents(
    renderer: &Renderer,
    doc: &Document,
    assets_dir: &Path,
) -> Result<Vec<SourceData>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    for (name, decl) in &doc.sources {
        let SourceDecl::GeoJson(g) = decl else {
            continue;
        };
        if g.data
            .as_ref()
            .is_some_and(|d| d.is_object() || d.is_array())
        {
            continue;
        }
        let Some(url) = g
            .url
            .as_deref()
            .or_else(|| g.data.as_ref().and_then(serde_json::Value::as_str))
        else {
            return Err(
                format!("geojson `{name}`: declares neither inline `data` nor a `url`").into(),
            );
        };
        let bytes = ezu::paint::host::read_asset_bytes(url, assets_dir)
            .await
            .map_err(|e| format!("geojson `{name}`: {e}"))?;
        out.push(renderer.load_source(name, &bytes)?);
    }
    Ok(out)
}

/// Whether to evaluate the graph on one thread, as `EZU_SERIAL=1`
/// requests. Single-threaded evaluation is what a WebAssembly host gets,
/// and it holds far fewer intermediates alive at once than the Rayon
/// scheduler does, so it is the mode to reach for when reproducing a
/// browser or Workers memory profile from the CLI.
fn serial_eval() -> bool {
    std::env::var("EZU_SERIAL")
        .map(|v| v != "0" && !v.is_empty())
        .unwrap_or(false)
}

/// Source tiles decoded once and kept for the renders that read them next.
///
/// Keyed by source, tile and a hash of the bytes, so a tile whose bytes
/// changed — an edited local file, which `ezu serve` reads afresh on every
/// request — is decoded again, and one whose bytes did not is never decoded
/// twice. Bounded by the size of the bytes each entry was decoded from;
/// past the budget, the entry used longest ago goes first. Renders that
/// want the same tile at once decode it once: the others wait for it.
pub(crate) struct DecodedCache {
    budget: usize,
    inner: Mutex<DecodedInner>,
}

type DecodedKey = (Arc<str>, TileId, u128);
type DecodedSlot = Arc<OnceLock<Result<SourceData, Error>>>;

#[derive(Default)]
struct DecodedInner {
    /// Each entry, the tick it was last used at, and what it is charged.
    entries: HashMap<DecodedKey, (DecodedSlot, u64, usize)>,
    /// The same entries by last use, oldest first.
    by_use: BTreeMap<u64, DecodedKey>,
    bytes: usize,
    tick: u64,
}

/// What an entry is charged beyond its bytes, so that a run of tiny tiles
/// cannot grow the cache without bound.
const DECODED_ENTRY_OVERHEAD: usize = 64;

impl DecodedCache {
    pub(crate) fn new(budget: usize) -> Self {
        Self {
            budget,
            inner: Mutex::new(DecodedInner::default()),
        }
    }

    /// `bytes` of `source`'s tile `tile`, decoded: from the cache, or
    /// decoded now and kept.
    fn load(
        &self,
        renderer: &Renderer,
        source: &Arc<str>,
        tile: TileId,
        bytes: &[u8],
    ) -> Result<SourceData, Error> {
        let key = (Arc::clone(source), tile, xxhash_rust::xxh3::xxh3_128(bytes));
        let slot = self.slot(key, bytes.len() + DECODED_ENTRY_OVERHEAD);
        slot.get_or_init(|| renderer.load_source(source, bytes))
            .clone()
    }

    /// The slot for `key`, made and charged `weight` when there is none.
    fn slot(&self, key: DecodedKey, weight: usize) -> DecodedSlot {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.tick += 1;
        let tick = inner.tick;
        if let Some((slot, used, _)) = inner.entries.get_mut(&key) {
            let (slot, old) = (Arc::clone(slot), std::mem::replace(used, tick));
            inner.by_use.remove(&old);
            inner.by_use.insert(tick, key);
            return slot;
        }
        let slot = DecodedSlot::default();
        if weight > self.budget {
            // Too big to keep: decode it for this render alone.
            return slot;
        }
        inner
            .entries
            .insert(key.clone(), (Arc::clone(&slot), tick, weight));
        inner.by_use.insert(tick, key);
        inner.bytes += weight;
        while inner.bytes > self.budget {
            let Some((_, oldest)) = inner.by_use.pop_first() else {
                break;
            };
            if let Some((_, _, gone)) = inner.entries.remove(&oldest) {
                inner.bytes -= gone;
            }
        }
        slot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STYLE: &str = r##"{
        "name": "t",
        "sources": { "basemap": { "type": "mvt", "url": "https://x/{z}/{x}/{y}.mvt" } },
        "nodes": { "out": { "op": "solid", "color": "#000000" } },
        "output": "@out"
    }"##;
    const Z14: &[u8] = include_bytes!("../../../go/testdata/basemap-14-14554-6454.mvt");
    const Z13: &[u8] = include_bytes!("../../../go/testdata/basemap-13-7277-3227.mvt");

    fn count(cache: &DecodedCache) -> usize {
        cache.inner.lock().unwrap().entries.len()
    }

    #[test]
    fn a_tile_decodes_once_until_its_bytes_change_or_it_ages_out() {
        let r = Renderer::new(STYLE).unwrap();
        let name: Arc<str> = Arc::from("basemap");
        let (a, b) = (TileId::new(14, 1, 1), TileId::new(14, 1, 2));
        let cache = DecodedCache::new(Z14.len() + Z13.len() + 2 * DECODED_ENTRY_OVERHEAD);

        let first = cache.load(&r, &name, a, Z14).unwrap();
        let again = cache.load(&r, &name, a, Z14).unwrap();
        assert_eq!(count(&cache), 1, "the same bytes are one entry");
        assert_eq!(format!("{first:?}"), format!("{again:?}"));
        // Other bytes for the same tile are another entry, as a tile
        // edited on disk is.
        cache.load(&r, &name, a, Z13).unwrap();
        assert_eq!(count(&cache), 2);
        // Past the budget the one used longest ago goes: `a` with Z14.
        cache.load(&r, &name, b, Z14).unwrap();
        assert_eq!(count(&cache), 2);
        let inner = cache.inner.lock().unwrap();
        let hash = xxhash_rust::xxh3::xxh3_128(Z14);
        assert!(!inner.entries.contains_key(&(Arc::clone(&name), a, hash)));
        assert!(inner.entries.contains_key(&(Arc::clone(&name), b, hash)));
    }

    #[test]
    fn a_tile_that_does_not_decode_fails_every_render_that_reads_it() {
        let r = Renderer::new(STYLE).unwrap();
        let name: Arc<str> = Arc::from("basemap");
        let cache = DecodedCache::new(1 << 20);
        let tile = TileId::new(14, 1, 1);
        assert!(cache.load(&r, &name, tile, b"not a tile").is_err());
        assert!(cache.load(&r, &name, tile, b"not a tile").is_err());
        // A tile bigger than the whole budget still renders.
        let small = DecodedCache::new(16);
        assert!(small.load(&r, &name, tile, Z14).is_ok());
        assert_eq!(count(&small), 0);
    }
}
