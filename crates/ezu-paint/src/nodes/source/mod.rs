//! Feature source ops — produce `Features` from host-bound asset
//! layers (MVT, GeoJSON, …) or synthesize them from style fields /
//! tile geometry.

mod dem;
mod features;
pub(crate) mod fonts;
mod graticule;
mod icon;
mod image;
mod literal_geometry;
mod point_grid;
mod point_scatter;
mod raster;
pub(crate) mod sprite;
mod tile_bounds;

use ezu_graph::{FactoryError, In, InReader};

/// Read a lattice's `spacing` / `spacing-x` / `spacing-y` trio. `spacing`
/// supplies whichever axis is not given its own, so it is only required
/// when one is missing. Each of the three takes a literal, a `$param` or an
/// `@node`; a shared `spacing` that is a port feeds both axes from that one
/// port.
fn read_spacing(
    r: &mut InReader<'_, '_>,
    fields: &serde_json::Map<String, serde_json::Value>,
) -> Result<(In<f64>, In<f64>), FactoryError> {
    // Read `spacing` whenever it is present, even if both axes override
    // it, so a malformed value is still reported rather than ignored.
    let shared = match fields.contains_key("spacing") {
        true => Some(r.number("spacing")?),
        false => None,
    };
    let mut axis = |name: &'static str| match (fields.contains_key(name), &shared) {
        (true, _) => r.number(name),
        (false, Some(s)) => Ok(s.clone()),
        (false, None) => Err(FactoryError::MissingField("spacing".into())),
    };
    let x = axis("spacing-x")?;
    let y = axis("spacing-y")?;
    Ok((x, y))
}

/// Schema for one per-axis override read by [`read_spacing`].
fn spacing_axis_schema(axis: &str) -> serde_json::Value {
    ezu_graph::schema_frag::in_number(serde_json::json!({
        "type": "number", "minimum": 0.0,
        "description": format!("Spacing along {axis} in extent units, overriding `spacing` on that axis."),
    }))
}

/// The `required` alternatives [`read_spacing`] accepts: `spacing`, or
/// both per-axis spacings.
fn spacing_any_of() -> serde_json::Value {
    serde_json::json!([
        { "required": ["spacing"] },
        { "required": ["spacing-x", "spacing-y"] },
    ])
}
