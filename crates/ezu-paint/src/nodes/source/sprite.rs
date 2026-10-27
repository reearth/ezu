//! What every reader of a sprite sheet shares, so the icon a name gives
//! cannot differ between them: [`sprite_atlas_key`] resolves a `sprite`
//! field to the sheet's asset key, and [`IconCrops`] crops named icons out
//! of a sheet. A name the sheet lacks draws no icon, wherever it is asked
//! for, and is reported once per node.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use ezu_graph::{Asset, EvalCtx, EvalError, FactoryCtx, FactoryError, RasterBuf, SpriteSheet};
use ezu_style as spec;

/// Resolve a `sprite` field — an `@sprite-source` reference, or a literal
/// atlas key — to the sheet's asset key (the source's `image`). `field`
/// names the field in error messages.
pub(crate) fn sprite_atlas_key(
    field: &str,
    raw: &str,
    ctx: &FactoryCtx<'_>,
) -> Result<String, FactoryError> {
    match spec::FieldRef::classify(raw) {
        spec::FieldRef::Node(name) => {
            let source = ctx
                .sources
                .get(name)
                .ok_or_else(|| FactoryError::UnknownAsset(name.to_string()))?;
            let spec::SourceDecl::Sprite(sprite) = source else {
                return Err(FactoryError::BadField {
                    field: field.into(),
                    msg: format!("source `{name}` is not a sprite"),
                });
            };
            Ok(sprite.image.clone())
        }
        spec::FieldRef::Literal(s) => Ok(s.to_string()),
        spec::FieldRef::Param(_) => Err(FactoryError::BadField {
            field: field.into(),
            msg: "a sprite cannot come from a param".into(),
        }),
    }
}

/// Load the sheet bound at `key`.
pub(crate) fn load_sheet(ctx: &EvalCtx<'_>, key: &str) -> Result<Arc<SpriteSheet>, EvalError> {
    match ctx.assets.load(key)? {
        Asset::Sprite(sheet) => Ok(sheet),
        _ => Err(EvalError::Other(format!(
            "asset `{key}` is not a sprite sheet"
        ))),
    }
}

/// Crops named icons out of a sheet, remembering which names the sheet
/// lacked so each is reported once per node rather than once per tile.
///
/// A name with no rect in the sheet, or one whose rect falls outside the
/// atlas, draws no icon: `stamp` and `line-stamp` skip the features naming
/// it, a label keeps its text without the icon, and `icon` hands on a
/// transparent 1×1 sprite, which draws nothing either.
#[derive(Default)]
pub(crate) struct IconCrops {
    reported: Mutex<HashSet<String>>,
}

impl IconCrops {
    /// The icon `name` cut out of `sheet`, or `None` (reported) when the
    /// sheet has no such icon. `op` names the asking op in the report.
    pub(crate) fn crop(&self, op: &str, sheet: &SpriteSheet, name: &str) -> Option<RasterBuf> {
        let img = sheet.crop(name);
        if img.is_none() {
            let mut reported = self.reported.lock().unwrap_or_else(|e| e.into_inner());
            if reported.insert(name.to_string()) {
                tracing::warn!("{op}: the sprite sheet has no icon `{name}` — none is drawn");
            }
        }
        img
    }
}
