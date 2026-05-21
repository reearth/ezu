//! `PortValue` — the runtime values flowing along DAG edges.

use std::sync::Arc;

use crate::buf::{OpaqueValue, RasterBuf};
use crate::port::PortKind;

/// One value flowing along an edge. Cloning is cheap (Arc / Copy).
#[derive(Debug, Clone)]
pub enum PortValue {
    Features(OpaqueValue),
    Raster(Arc<RasterBuf>),
    Brush(OpaqueValue),
    Scalar(ScalarValue),
}

impl PortValue {
    pub fn kind(&self) -> PortKind {
        match self {
            PortValue::Features(_) => PortKind::Features,
            PortValue::Raster(_) => PortKind::Raster,
            PortValue::Brush(_) => PortKind::Brush,
            PortValue::Scalar(_) => PortKind::Scalar,
        }
    }

    pub fn as_raster(&self) -> Option<&Arc<RasterBuf>> {
        if let PortValue::Raster(r) = self {
            Some(r)
        } else {
            None
        }
    }

    pub fn as_scalar(&self) -> Option<&ScalarValue> {
        if let PortValue::Scalar(s) = self {
            Some(s)
        } else {
            None
        }
    }
}

/// A constant value carried on a `Scalar` port.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScalarValue {
    /// Linear sRGB RGBA, components in `[0, 1]`.
    Color([f32; 4]),
    Number(f64),
    Bool(bool),
}

impl ScalarValue {
    pub fn as_color(&self) -> Option<[f32; 4]> {
        if let ScalarValue::Color(c) = self {
            Some(*c)
        } else {
            None
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        if let ScalarValue::Number(n) = self {
            Some(*n)
        } else {
            None
        }
    }
}
