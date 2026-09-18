//! Reading option objects out of JSON.
//!
//! The JS shell reads the same options off a JS object; here they arrive as
//! JSON text. The field names, the accepted values and — where a value is
//! refused — the [`ErrorKind`] are the same in both, so a caller porting
//! between the two shells changes the syntax and nothing else.
//!
//! Unknown keys are ignored, so a field added later does not break an older
//! host. Values that are present but wrong are refused: silently answering
//! PNG to `"format": "jpeg"` hands back bytes the caller was not expecting.

use ezu_renderer::{BindOptions, Error, ErrorKind, OutputFormat, PngCompression, RenderOptions};
use serde_json::Value;

fn err(kind: ErrorKind, message: impl std::fmt::Display) -> Error {
    Error::new(kind, message)
}

/// Parse an options payload, treating an empty one as `{}`.
///
/// Malformed JSON is `InvalidStyle`. The JS shell has no equivalent failure
/// — a JS object either is one or is not — so this is the one option error
/// that has no counterpart there.
fn object(raw: &[u8], what: &str) -> Result<serde_json::Map<String, Value>, Error> {
    if raw.is_empty() {
        return Ok(serde_json::Map::new());
    }
    match serde_json::from_slice::<Value>(raw).map_err(|e| err(ErrorKind::InvalidStyle, e))? {
        Value::Object(map) => Ok(map),
        Value::Null => Ok(serde_json::Map::new()),
        other => Err(err(
            ErrorKind::InvalidStyle,
            format!("{what} must be a JSON object, got {other}"),
        )),
    }
}

/// `{"coord": [dx, dy], "sourceZoom": <z>, "index": "<sprite json text>"}`.
pub fn parse_bind(raw: &[u8]) -> Result<BindOptions, Error> {
    let obj = object(raw, "the bind options")?;
    Ok(BindOptions {
        coord: parse_coord(obj.get("coord"))?,
        source_zoom: parse_source_zoom(obj.get("sourceZoom"))?,
        index: obj.get("index").and_then(Value::as_str).map(str::to_string),
    })
}

/// Which tile of the 3×3 neighbourhood the bytes are. Refused values carry
/// `UnknownSource`, matching the JS shell.
fn parse_coord(value: Option<&Value>) -> Result<(i32, i32), Error> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok((0, 0));
    };
    let arr = value.as_array().ok_or_else(|| {
        err(
            ErrorKind::UnknownSource,
            "coord must be a [dx, dy] array of two numbers",
        )
    })?;
    if arr.len() != 2 {
        return Err(err(
            ErrorKind::UnknownSource,
            "coord must have exactly two numbers [dx, dy]",
        ));
    }
    let axis = |i: usize, label: &str| -> Result<i32, Error> {
        arr[i]
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .ok_or_else(|| {
                err(
                    ErrorKind::UnknownSource,
                    format!("coord[{i}] ({label}) must be a number"),
                )
            })
    };
    let dx = axis(0, "dx")?;
    let dy = axis(1, "dy")?;
    // Only the 3×3 neighbourhood is ever stitched or collided against, so
    // anything further out would be accepted, stored, and never read.
    if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) {
        return Err(err(
            ErrorKind::UnknownSource,
            format!("coord [{dx}, {dy}] is outside the 3×3 neighbourhood (dx, dy ∈ -1..=1)"),
        ));
    }
    Ok((dx, dy))
}

fn parse_source_zoom(value: Option<&Value>) -> Result<Option<u8>, Error> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let z = value.as_u64().filter(|z| *z <= 30).ok_or_else(|| {
        err(
            ErrorKind::UnknownSource,
            format!("sourceZoom must be a whole zoom level in 0..=30, got {value}"),
        )
    })?;
    Ok(Some(z as u8))
}

/// `{"format": …, "tileSize": …, "pad": …, "png": {"compression": …},
/// "params": {…}}`.
pub fn parse_render(raw: &[u8]) -> Result<RenderOptions, Error> {
    let obj = object(raw, "the render options")?;
    let mut out = RenderOptions::default();

    if let Some(v) = obj.get("format").filter(|v| !v.is_null()) {
        out.format = match v.as_str() {
            Some("png") => OutputFormat::Png,
            Some("webp") => OutputFormat::Webp,
            Some("rgba") => OutputFormat::Rgba,
            _ => {
                return Err(err(
                    ErrorKind::InvalidStyle,
                    format!("format must be \"png\", \"webp\" or \"rgba\", got {v}"),
                ))
            }
        };
    }
    out.tile_size = parse_u32(obj.get("tileSize"), "tileSize")?;
    out.pad = parse_u32(obj.get("pad"), "pad")?;

    if let Some(png) = obj.get("png").and_then(Value::as_object) {
        if let Some(v) = png.get("compression").filter(|v| !v.is_null()) {
            out.png_compression = match v.as_str() {
                Some("fast") => PngCompression::Fast,
                Some("default") => PngCompression::Default,
                Some("best") => PngCompression::Best,
                _ => {
                    return Err(err(
                        ErrorKind::InvalidStyle,
                        format!(
                            "png.compression must be \"fast\", \"default\" or \"best\", got {v}"
                        ),
                    ))
                }
            };
        }
    }

    // `parallel` is not read. There is one thread in a wasip1 instance, so
    // the parallel evaluator has no pool to reach for; a host that wants
    // several tiles at once runs several instances, which is the same
    // concurrency in a place where it is safe.

    if let Some(params) = obj.get("params").and_then(Value::as_object) {
        for (name, value) in params {
            // Numbers and bools go through text rather than being matched
            // on: `parse_param_value` inside the renderer owns the coercion
            // rules, and stringifying keeps this shell from inventing its
            // own.
            let raw = match value {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => continue,
            };
            out.params.push((name.clone(), raw));
        }
    }
    Ok(out)
}

fn parse_u32(value: Option<&Value>, label: &str) -> Result<Option<u32>, Error> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let n = value
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| {
            err(
                ErrorKind::InvalidStyle,
                format!("{label} must be a non-negative whole number, got {value}"),
            )
        })?;
    Ok(Some(n))
}
