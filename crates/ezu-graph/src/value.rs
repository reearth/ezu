//! `PortValue` — the runtime values flowing along DAG edges.

use std::sync::Arc;

use crate::buf::{OpaqueValue, RasterBuf, ScalarField};
use crate::port::PortKind;

/// One value flowing along an edge. Cloning is cheap (Arc / Copy).
#[derive(Debug, Clone)]
pub enum PortValue {
    Features(OpaqueValue),
    Raster(Arc<RasterBuf>),
    Sprite(Arc<RasterBuf>),
    Brush(OpaqueValue),
    /// Label placement candidates or decisions (see [`PortKind::Labels`]).
    Labels(OpaqueValue),
    Scalar(ScalarValue),
    ScalarField(Arc<ScalarField>),
}

impl PortValue {
    pub fn kind(&self) -> PortKind {
        match self {
            PortValue::Features(_) => PortKind::Features,
            PortValue::Raster(_) => PortKind::Raster,
            PortValue::Sprite(_) => PortKind::Sprite,
            PortValue::Brush(_) => PortKind::Brush,
            PortValue::Labels(_) => PortKind::Labels,
            PortValue::Scalar(_) => PortKind::Scalar,
            PortValue::ScalarField(_) => PortKind::ScalarField,
        }
    }

    pub fn as_scalar_field(&self) -> Option<&Arc<ScalarField>> {
        if let PortValue::ScalarField(f) = self {
            Some(f)
        } else {
            None
        }
    }

    pub fn as_raster(&self) -> Option<&Arc<RasterBuf>> {
        if let PortValue::Raster(r) = self {
            Some(r)
        } else {
            None
        }
    }

    pub fn as_sprite(&self) -> Option<&Arc<RasterBuf>> {
        if let PortValue::Sprite(s) = self {
            Some(s)
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
    /// Straight (non-premultiplied) sRGB-encoded RGBA, components in
    /// `[0, 1]` — the same convention as a parsed `#rrggbb[aa]`
    /// literal. Consumers linearize / premultiply as needed.
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

    pub fn as_bool(&self) -> Option<bool> {
        if let ScalarValue::Bool(b) = self {
            Some(*b)
        } else {
            None
        }
    }

    /// Short kind name for error messages (`number` / `color` / `bool`).
    pub fn kind_name(&self) -> &'static str {
        match self {
            ScalarValue::Color(_) => "color",
            ScalarValue::Number(_) => "number",
            ScalarValue::Bool(_) => "bool",
        }
    }

    /// Feed this value into a cache-key hasher. Stable across runs.
    pub fn hash_into(&self, h: &mut xxhash_rust::xxh3::Xxh3) {
        match self {
            ScalarValue::Color(c) => {
                h.update(b"C");
                for ch in c {
                    h.update(&ch.to_le_bytes());
                }
            }
            ScalarValue::Number(n) => {
                h.update(b"N");
                h.update(&n.to_le_bytes());
            }
            ScalarValue::Bool(b) => {
                h.update(b"B");
                h.update(&[*b as u8]);
            }
        }
    }
}
