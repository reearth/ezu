//! Shared `kind: raster | sprite | scalar` parsing for synthetic
//! generator nodes (`solid`, `circle`, `gradient-*`, `noise`). They all
//! follow the same pattern: a single `kind` enum plus an optional
//! `width-px` / `height-px` pair that only matters in sprite mode.
//!
//! - `raster` (default) fills the padded canvas, positioned in tile or
//!   world coordinates.
//! - `sprite` draws a self-contained `width-px × height-px` image, for
//!   `stamp`, `tiling` or `place` to put somewhere.
//! - `scalar` emits a canvas-sized `ScalarField` of the number the
//!   generator computes before it picks a colour — a gradient's position,
//!   a circle's coverage, noise's raw value — for `color-ramp`,
//!   `threshold`, `field-math` or a displacement to take up.

use ezu_graph::{read_enum_baked, EnumTable, FactoryCtx, FactoryError, PortKind};
use serde_json::Value;

use crate::nodes::common::read_number_or;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GeneratorKind {
    Raster,
    Sprite { width: u32, height: u32 },
    Scalar,
}

impl GeneratorKind {
    pub(super) fn hash_tag(self) -> ([u8; 1], Option<(u32, u32)>) {
        match self {
            GeneratorKind::Raster => (*b"r", None),
            GeneratorKind::Sprite { width, height } => (*b"s", Some((width, height))),
            GeneratorKind::Scalar => (*b"f", None),
        }
    }

    /// The port kind a node of this kind produces.
    pub(super) fn port_kind(self) -> PortKind {
        match self {
            GeneratorKind::Raster => PortKind::Raster,
            GeneratorKind::Sprite { .. } => PortKind::Sprite,
            GeneratorKind::Scalar => PortKind::ScalarField,
        }
    }
}

/// What `kind` names, before a sprite's size is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KindName {
    Raster,
    Sprite,
    Scalar,
}

/// `kind` decides the node's output port, so it is read at build time.
/// `scalar-field` is another name for `scalar`.
const KINDS: EnumTable<KindName> = &[
    ("raster", KindName::Raster),
    ("sprite", KindName::Sprite),
    ("scalar", KindName::Scalar),
    ("scalar-field", KindName::Scalar),
];

/// The schema of the `kind` field [`parse_generator_kind`] reads.
pub(super) fn generator_kind_schema() -> Value {
    ezu_graph::schema_frag::in_enum(serde_json::json!({
        "type": "string",
        "enum": ["raster", "sprite", "scalar"],
        "default": "raster",
    }))
}

/// Parse the `kind` field plus `width-px` / `height-px` (required when
/// `kind = "sprite"`). Defaults to `Raster`; height defaults to width.
pub(super) fn parse_generator_kind(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
) -> Result<GeneratorKind, FactoryError> {
    match read_enum_baked(fields, ctx, "kind", KindName::Raster, KINDS)? {
        KindName::Raster => Ok(GeneratorKind::Raster),
        KindName::Scalar => Ok(GeneratorKind::Scalar),
        KindName::Sprite => {
            let w = read_number_or(fields, "width-px", ctx, 0.0)?.round();
            if w < 1.0 {
                return Err(FactoryError::BadField {
                    field: "width-px".into(),
                    msg: "sprite mode requires width-px ≥ 1".into(),
                });
            }
            let h = read_number_or(fields, "height-px", ctx, w)?.round();
            if h < 1.0 {
                return Err(FactoryError::BadField {
                    field: "height-px".into(),
                    msg: "must be ≥ 1".into(),
                });
            }
            Ok(GeneratorKind::Sprite {
                width: w as u32,
                height: h as u32,
            })
        }
    }
}
