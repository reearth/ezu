# ezu-pmtiles

PMTiles reader for the [`ezu`](../../README.md) workspace.

Thin async wrapper around the [`pmtiles`](https://crates.io/crates/pmtiles)
crate that returns the decompressed MVT bytes for an
[`ezu_core::TileId`] from either a local file (`mmap`) or a remote URL
(HTTP range requests).

## API

```rust
pub struct PmTilesArchive { /* … */ }

impl PmTilesArchive {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, PmTilesError>;
    pub async fn open_url(url: &str)         -> Result<Self, PmTilesError>;
    pub fn       header(&self)               -> &pmtiles::Header;
    pub async fn get_tile(&self, tile: TileId) -> Result<Option<Bytes>, PmTilesError>;
}
```

`get_tile` returns `Ok(None)` for tiles that aren't present in the archive
(rather than an error), matching how Protomaps' upstream serves "no data"
tiles. Decompression (gzip / brotli / zstd, per the archive header) is
handled internally.

## Example

```rust
let archive = ezu_pmtiles::PmTilesArchive::open_url(
    "https://build.protomaps.com/20260520.pmtiles"
).await?;
if let Some(bytes) = archive.get_tile(ezu_core::TileId::new(13, 7276, 3225)).await? {
    let decoded = ezu_mvt::decode(&bytes)?;
    // …render…
}
```

`ezu-pmtiles` requires a Tokio runtime; it's a strictly async crate.

See the main [README](../../README.md) for the full project overview.

## License

MIT or Apache-2.0, at your option.
