//! Host-side glue for rendering: pluggable `AssetLoader` impls and
//! conversion helpers between `ezu_graph::RasterBuf` and `tiny-skia` /
//! PNG output.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ezu_graph::{Asset, AssetError, AssetLoader, RasterBuf};
use hokusai::Brush;
use tiny_skia::{Pixmap, PixmapPaint, Transform};

use crate::PaintError;

/// In-memory brush bank that satisfies `brush-file` references.
///
/// Names may be supplied with or without a leading `@`. If a name is not
/// in the in-memory map, the loader falls back to reading
/// `<brushes_dir>/<name>.myb` from disk.
pub struct BrushBankLoader {
    pub bank: HashMap<String, Arc<Brush>>,
    pub brushes_dir: Option<PathBuf>,
}

impl BrushBankLoader {
    pub fn new() -> Self {
        Self {
            bank: HashMap::new(),
            brushes_dir: None,
        }
    }

    pub fn with_dir(mut self, dir: PathBuf) -> Self {
        self.brushes_dir = Some(dir);
        self
    }

    pub fn insert(&mut self, name: impl Into<String>, brush: Brush) {
        self.bank.insert(name.into(), Arc::new(brush));
    }
}

impl Default for BrushBankLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl AssetLoader for BrushBankLoader {
    fn load(&self, src: &str) -> Result<Asset, AssetError> {
        let key = src.strip_prefix('@').unwrap_or(src);
        if let Some(b) = self.bank.get(key) {
            return Ok(Asset::Brush(b.clone()));
        }
        if let Some(dir) = &self.brushes_dir {
            // Try `<dir>/<key>` then `<dir>/<key>.myb`.
            let candidates = [dir.join(key), dir.join(format!("{key}.myb"))];
            for path in &candidates {
                if path.exists() {
                    let bytes = std::fs::read_to_string(path).map_err(|e| AssetError::Decode {
                        src: src.to_string(),
                        msg: e.to_string(),
                    })?;
                    let brush = hokusai::myb::from_str(&bytes).map_err(|e| AssetError::Decode {
                        src: src.to_string(),
                        msg: e.to_string(),
                    })?;
                    return Ok(Asset::Brush(Arc::new(brush)));
                }
            }
        }
        Err(AssetError::NotFound(src.to_string()))
    }
}

/// Crop a padded raster down to the central `tile_size` × `tile_size`
/// region and encode as PNG.
pub fn raster_to_png(
    buf: &RasterBuf,
    tile_size: u32,
    pad: u32,
) -> Result<Vec<u8>, PaintError> {
    // Wrap our RGBA8 premul buffer as a tiny-skia Pixmap by copying. We
    // could share memory but the API requires a Pixmap-owned allocation.
    let padded = pixmap_from_raster(buf)?;
    if pad == 0 && padded.width() == tile_size && padded.height() == tile_size {
        return padded.encode_png().map_err(|_| PaintError::PngEncode);
    }
    let mut out = Pixmap::new(tile_size, tile_size).ok_or(PaintError::PngEncode)?;
    out.draw_pixmap(
        -(pad as i32),
        -(pad as i32),
        padded.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    out.encode_png().map_err(|_| PaintError::PngEncode)
}

fn pixmap_from_raster(buf: &RasterBuf) -> Result<Pixmap, PaintError> {
    let mut p = Pixmap::new(buf.width, buf.height).ok_or(PaintError::PngEncode)?;
    p.data_mut().copy_from_slice(&buf.pixels);
    Ok(p)
}

/// Crop a padded raster down to the central tile region and return
/// straight (un-premultiplied) 8-bit RGBA bytes — directly compatible
/// with `new ImageData(new Uint8ClampedArray(...), w, h)` in JS.
pub fn raster_to_rgba8(buf: &RasterBuf, tile_size: u32, pad: u32) -> Vec<u8> {
    let padded = match pixmap_from_raster(buf) {
        Ok(p) => p,
        Err(_) => return vec![0; (tile_size * tile_size * 4) as usize],
    };
    let tile_pixmap = if pad == 0 && padded.width() == tile_size && padded.height() == tile_size {
        padded
    } else {
        let mut out = match Pixmap::new(tile_size, tile_size) {
            Some(p) => p,
            None => return vec![0; (tile_size * tile_size * 4) as usize],
        };
        out.draw_pixmap(
            -(pad as i32),
            -(pad as i32),
            padded.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        out
    };
    let mut rgba = Vec::with_capacity((tile_size * tile_size * 4) as usize);
    for p in tile_pixmap.pixels() {
        let p = p.demultiply();
        rgba.extend_from_slice(&[p.red(), p.green(), p.blue(), p.alpha()]);
    }
    rgba
}
