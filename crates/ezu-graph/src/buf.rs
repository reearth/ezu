//! Concrete buffer types flowing along `Raster` edges.
//!
//! These are deliberately small and dependency-free so node
//! implementations from different crates can produce / consume them
//! without a shared dependency on `tiny-skia` or `hokusai`. Nodes
//! that wrap those engines do conversions at their boundaries.

use std::any::Any;
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

/// Type-erased value carried on `Features` and `Brush` ports. Concrete
/// types are a convention between producer and consumer node impls;
/// downcasts happen inside nodes. The DAG only checks the `PortKind`.
pub type OpaqueValue = Arc<dyn Any + Send + Sync>;

/// Per-pixel elevation grid flowing along `HeightField` ports.
///
/// Layout is row-major, one `f32` per pixel, in **metres above ellipsoid**
/// (or whatever the source declares — the host owns the datum). `width` /
/// `height` MUST match the canvas's `padded_size()` so consumers (e.g.
/// `hillshade`) can pair samples with the same geometry as their raster
/// output. Missing samples (e.g. ocean nodata in some DEMs) surface as
/// `nodata`; consumers fall back to `0.0` or pass-through.
///
/// `metres_per_pixel_x` / `_y` are filled by the producer from tile
/// geometry and latitude (Web Mercator scale is latitude-dependent), so
/// gradient-based consumers can produce geographically faithful slopes
/// without re-deriving the tile geometry.
#[derive(Debug, Clone)]
pub struct HeightField {
    pub width: u32,
    pub height: u32,
    pub metres_per_pixel_x: f32,
    pub metres_per_pixel_y: f32,
    pub elev: Arc<[f32]>,
    pub nodata: Option<f32>,
}

impl HeightField {
    pub fn sample(&self, x: u32, y: u32) -> f32 {
        self.elev[(y * self.width + x) as usize]
    }
}
