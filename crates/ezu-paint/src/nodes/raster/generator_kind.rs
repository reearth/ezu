//! Shared `kind: raster | sprite` parsing for synthetic generator
//! nodes (`solid`, `circle`, `gradient-*`). All four follow the same
//! pattern: a single `kind` enum + an optional `width-px` / `height-px`
//! pair that only matters in sprite mode.

use ezu_graph::{FactoryCtx, FactoryError};
use serde_json::Value;

use crate::nodes::common::{read_number_or, read_optional_string};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GeneratorKind {
    Raster,
    Sprite { width: u32, height: u32 },
}

impl GeneratorKind {
    pub(super) fn hash_tag(self) -> ([u8; 1], Option<(u32, u32)>) {
        match self {
            GeneratorKind::Raster => (*b"r", None),
            GeneratorKind::Sprite { width, height } => (*b"s", Some((width, height))),
        }
    }
}

/// Parse the `kind` field plus `width-px` / `height-px` (required when
/// `kind = "sprite"`). Defaults to `Raster`; height defaults to width.
pub(super) fn parse_generator_kind(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
) -> Result<GeneratorKind, FactoryError> {
    match read_optional_string(fields, "kind")?.as_deref() {
        None | Some("raster") => Ok(GeneratorKind::Raster),
        Some("sprite") => {
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
        Some(other) => Err(FactoryError::BadField {
            field: "kind".into(),
            msg: format!("expected `raster` or `sprite`, got `{other}`"),
        }),
    }
}
