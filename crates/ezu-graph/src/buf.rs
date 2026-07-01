//! Concrete buffer types flowing along `Raster` edges.
//!
//! These are deliberately small and dependency-free so node
//! implementations from different crates can produce / consume them
//! without a shared dependency on `tiny-skia` or `hokusai`. Nodes
//! that wrap those engines do conversions at their boundaries.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

/// RGBA8 raster, sRGB color space, premultiplied alpha. Layout is
/// row-major, four bytes per pixel `[R, G, B, A]`.
#[derive(Debug, Clone)]
pub struct RasterBuf {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl RasterBuf {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; (width * height * 4) as usize],
        }
    }

    pub fn filled(width: u32, height: u32, rgba: [u8; 4]) -> Self {
        let mut s = Self::new(width, height);
        for px in s.pixels.chunks_exact_mut(4) {
            px.copy_from_slice(&rgba);
        }
        s
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }
}

/// A sub-rectangle of a sprite atlas: one named icon.
#[derive(Debug, Clone, Copy)]
pub struct SpriteRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Device pixels per logical pixel the icon was authored at (a `@2x`
    /// sprite has `pixel_ratio == 2.0`). Consumers divide by it to get the
    /// icon's intended display size.
    pub pixel_ratio: f32,
}

/// A decoded sprite sheet: one atlas image plus a name → sub-rect index.
/// The runtime counterpart of a `sprite` source — the host builds it from
/// the atlas PNG and the (inline or fetched) index, and the `icon` node
/// crops named rects out of it.
#[derive(Debug)]
pub struct SpriteSheet {
    pub atlas: RasterBuf,
    pub icons: HashMap<String, SpriteRect>,
}

impl SpriteSheet {
    /// Crop a named icon out of the atlas into a standalone `RasterBuf`.
    /// Returns `None` if the name is unknown or its rect falls outside the
    /// atlas bounds.
    pub fn crop(&self, name: &str) -> Option<RasterBuf> {
        let r = self.icons.get(name)?;
        if r.width == 0
            || r.height == 0
            || r.x + r.width > self.atlas.width
            || r.y + r.height > self.atlas.height
        {
            return None;
        }
        let mut out = RasterBuf::new(r.width, r.height);
        let aw = self.atlas.width as usize;
        for row in 0..r.height {
            let src = (((r.y + row) as usize * aw) + r.x as usize) * 4;
            let dst = (row as usize * r.width as usize) * 4;
            let n = r.width as usize * 4;
            out.pixels[dst..dst + n].copy_from_slice(&self.atlas.pixels[src..src + n]);
        }
        Some(out)
    }
}

/// Type-erased value carried on `Features` and `Brush` ports. Concrete
/// types are a convention between producer and consumer node impls;
/// downcasts happen inside nodes. The DAG only checks the `PortKind`.
pub type OpaqueValue = Arc<dyn Any + Send + Sync>;

/// Per-pixel `f32` scalar grid flowing along `ScalarField` ports.
///
/// The general carrier for single-channel floating-point data —
/// elevation, signed distance, scalar noise, slope angle, anything
/// "one number per pixel". Layout is row-major, one `f32` per pixel.
/// `width` / `height` MUST match the canvas's `padded_size()` so
/// consumers can pair samples with the same geometry as their raster
/// output.
///
/// `geo_scale` is populated when the values represent a quantity
/// measured per real-world distance (e.g. elevation in metres at a
/// particular latitude). Gradient-based consumers (`hillshade`,
/// `slope`) read it to compute geographically faithful results.
/// `None` means the field is unitless / in pixel space — fine for
/// `color-ramp` style mapping but stylization-only
/// for gradient ops.
///
/// Missing samples (e.g. ocean nodata in some DEMs) surface as
/// `nodata`; consumers fall back to `0.0` or pass-through.
#[derive(Debug, Clone)]
pub struct ScalarField {
    pub width: u32,
    pub height: u32,
    pub values: Arc<[f32]>,
    pub nodata: Option<f32>,
    pub geo_scale: Option<GeoScale>,
}

/// Geographic per-pixel scaling for a `ScalarField`. Filled by the
/// producer from tile geometry and latitude (Web Mercator's scale is
/// latitude-dependent), so consumers like `slope` don't need to
/// re-derive tile geometry.
#[derive(Debug, Clone, Copy)]
pub struct GeoScale {
    pub metres_per_pixel_x: f32,
    pub metres_per_pixel_y: f32,
}

impl ScalarField {
    pub fn sample(&self, x: u32, y: u32) -> f32 {
        self.values[(y * self.width + x) as usize]
    }

    /// Real-world metres per pixel along X, or `1.0` when the field
    /// has no geographic scaling. Lets gradient consumers stay
    /// branch-free; the fallback is a no-op scaling that produces
    /// pixel-space gradients — geographically inaccurate but useful
    /// for stylization over non-DEM inputs.
    pub fn metres_per_pixel_x(&self) -> f32 {
        self.geo_scale.map(|g| g.metres_per_pixel_x).unwrap_or(1.0)
    }

    pub fn metres_per_pixel_y(&self) -> f32 {
        self.geo_scale.map(|g| g.metres_per_pixel_y).unwrap_or(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprite_crop_extracts_named_rect() {
        // 4×2 atlas: left half red, right half green (premultiplied, opaque).
        let mut atlas = RasterBuf::new(4, 2);
        for y in 0..2 {
            for x in 0..4 {
                let i = ((y * 4 + x) * 4) as usize;
                let c = if x < 2 {
                    [255, 0, 0, 255]
                } else {
                    [0, 255, 0, 255]
                };
                atlas.pixels[i..i + 4].copy_from_slice(&c);
            }
        }
        let mut icons = HashMap::new();
        icons.insert(
            "left".to_string(),
            SpriteRect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
                pixel_ratio: 1.0,
            },
        );
        icons.insert(
            "right".to_string(),
            SpriteRect {
                x: 2,
                y: 0,
                width: 2,
                height: 2,
                pixel_ratio: 1.0,
            },
        );
        icons.insert(
            "oob".to_string(),
            SpriteRect {
                x: 3,
                y: 0,
                width: 2,
                height: 2,
                pixel_ratio: 1.0,
            },
        );
        let sheet = SpriteSheet { atlas, icons };

        let right = sheet.crop("right").expect("named icon");
        assert_eq!((right.width, right.height), (2, 2));
        assert!(right.pixels.chunks_exact(4).all(|p| p == [0, 255, 0, 255]));

        let left = sheet.crop("left").unwrap();
        assert!(left.pixels.chunks_exact(4).all(|p| p == [255, 0, 0, 255]));

        // Unknown name / out-of-bounds rect → None.
        assert!(sheet.crop("missing").is_none());
        assert!(sheet.crop("oob").is_none());
    }
}
