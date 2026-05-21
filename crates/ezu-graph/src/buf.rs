//! Concrete buffer types flowing along `Raster` and `Mask` edges.
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

/// Single-channel mask in `[0, 1]`. Row-major, one f32 per pixel.
#[derive(Debug, Clone)]
pub struct MaskBuf {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<f32>,
}

impl MaskBuf {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0.0; (width * height) as usize],
        }
    }

    pub fn filled(width: u32, height: u32, value: f32) -> Self {
        Self {
            width,
            height,
            pixels: vec![value; (width * height) as usize],
        }
    }

    pub fn pixel(&self, x: u32, y: u32) -> f32 {
        self.pixels[(y * self.width + x) as usize]
    }
}

/// Type-erased value carried on `Features` and `Brush` ports. Concrete
/// types are a convention between producer and consumer node impls;
/// downcasts happen inside nodes. The DAG only checks the `PortKind`.
pub type OpaqueValue = Arc<dyn Any + Send + Sync>;
