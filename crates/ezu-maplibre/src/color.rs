//! MapLibre colour strings → ezu `#rrggbb` hex + separate alpha.
//!
//! ezu paint fields take a `#hex` colour and carry opacity in a sibling
//! field (`fill-alpha`, `opacity`), so we split alpha out here.

use serde_json::Value;

/// Parse a MapLibre colour into `(#rrggbb, alpha)`. Accepts `#rgb`,
/// `#rrggbb`, `#rrggbbaa`, `rgb(...)`, `rgba(...)`, and `hsl(a)(...)`.
/// Returns `None` for anything unrecognised (e.g. a bare expression).
pub fn parse_color(s: &str) -> Option<(String, f32)> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex(hex);
    }
    if let Some(inner) = s.strip_prefix("rgba(").and_then(|x| x.strip_suffix(')')) {
        return parse_rgb_components(inner, true);
    }
    if let Some(inner) = s.strip_prefix("rgb(").and_then(|x| x.strip_suffix(')')) {
        return parse_rgb_components(inner, false);
    }
    if let Some(inner) = s.strip_prefix("hsla(").and_then(|x| x.strip_suffix(')')) {
        return parse_hsl_components(inner, true);
    }
    if let Some(inner) = s.strip_prefix("hsl(").and_then(|x| x.strip_suffix(')')) {
        return parse_hsl_components(inner, false);
    }
    None
}

/// Parse a MapLibre colour into non-premultiplied RGBA floats (0..1), for
/// colour-space interpolation of zoom ramps.
pub fn parse_rgba(s: &str) -> Option<[f32; 4]> {
    let (hex, a) = parse_color(s)?;
    let bytes = hex.strip_prefix('#')?;
    let r = u8::from_str_radix(bytes.get(0..2)?, 16).ok()? as f32 / 255.0;
    let g = u8::from_str_radix(bytes.get(2..4)?, 16).ok()? as f32 / 255.0;
    let b = u8::from_str_radix(bytes.get(4..6)?, 16).ok()? as f32 / 255.0;
    Some([r, g, b, a])
}

/// Format non-premultiplied RGBA floats as `#rrggbb` (or `#rrggbbaa` when
/// alpha < 1) — the inverse of [`parse_rgba`] for emitting baked colours.
pub fn rgba_to_hex(c: [f32; 4]) -> String {
    let to = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let (r, g, b) = (to(c[0]), to(c[1]), to(c[2]));
    if c[3] >= 1.0 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{:02x}", to(c[3]))
    }
}

fn parse_hex(hex: &str) -> Option<(String, f32)> {
    let bytes: Vec<u8> = match hex.len() {
        3 => hex
            .chars()
            .flat_map(|c| [c, c])
            .collect::<String>()
            .into_bytes(),
        6 | 8 => hex.bytes().collect(),
        _ => return None,
    };
    let get = |i: usize| -> Option<u8> {
        let h = std::str::from_utf8(&bytes[i..i + 2]).ok()?;
        u8::from_str_radix(h, 16).ok()
    };
    let (r, g, b) = (get(0)?, get(2)?, get(4)?);
    let a = if bytes.len() == 8 {
        get(6)? as f32 / 255.0
    } else {
        1.0
    };
    Some((format!("#{r:02x}{g:02x}{b:02x}"), a))
}

fn parse_rgb_components(inner: &str, has_alpha: bool) -> Option<(String, f32)> {
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    if parts.len() < 3 {
        return None;
    }
    let chan = |p: &str| -> Option<u8> {
        // Support "255" and "100%".
        if let Some(pct) = p.strip_suffix('%') {
            let v: f32 = pct.trim().parse().ok()?;
            Some((v / 100.0 * 255.0).round().clamp(0.0, 255.0) as u8)
        } else {
            let v: f32 = p.parse().ok()?;
            Some(v.round().clamp(0.0, 255.0) as u8)
        }
    };
    let (r, g, b) = (chan(parts[0])?, chan(parts[1])?, chan(parts[2])?);
    let a = if has_alpha && parts.len() >= 4 {
        parts[3].parse::<f32>().ok()?.clamp(0.0, 1.0)
    } else {
        1.0
    };
    Some((format!("#{r:02x}{g:02x}{b:02x}"), a))
}

fn parse_hsl_components(inner: &str, has_alpha: bool) -> Option<(String, f32)> {
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    if parts.len() < 3 {
        return None;
    }
    let h: f32 = parts[0].parse().ok()?;
    let s: f32 = parts[1].trim_end_matches('%').parse::<f32>().ok()? / 100.0;
    let l: f32 = parts[2].trim_end_matches('%').parse::<f32>().ok()? / 100.0;
    let a = if has_alpha && parts.len() >= 4 {
        parts[3].parse::<f32>().ok()?.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let (r, g, b) = hsl_to_rgb(h, s, l);
    Some((format!("#{r:02x}{g:02x}{b:02x}"), a))
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to(r1), to(g1), to(b1))
}

/// A `["match", ["get", key], v_or_arr, color, ..., fallback]` expression
/// decomposed into filter buckets. Each arm's `values` is an ezu filter
/// match value (a single literal or a membership array).
pub struct MatchBuckets {
    pub key: String,
    pub arms: Vec<(Value, String)>,
    pub fallback: String,
}

/// Recognise `["match", ["get", <key>], label, out, ..., fallback]` where
/// every `out` is a colour literal. Returns `None` for any other shape
/// (e.g. `match` on a non-`get` input, or non-colour outputs).
pub fn match_buckets(expr: &Value) -> Option<MatchBuckets> {
    let arr = expr.as_array()?;
    if arr.first()?.as_str()? != "match" {
        return None;
    }
    // input must be ["get", "<key>"]
    let input = arr.get(1)?.as_array()?;
    if input.first()?.as_str()? != "get" {
        return None;
    }
    let key = input.get(1)?.as_str()?.to_string();

    // arms are (label, output) pairs between index 2 and the last element;
    // the final element is the fallback output.
    let body = &arr[2..];
    if body.len() < 3 || body.len() % 2 == 0 {
        // Need an odd count: N pairs + 1 fallback.
        return None;
    }
    let fallback = body.last()?.as_str()?.to_string();
    let mut arms = Vec::new();
    let mut i = 0;
    while i + 1 < body.len() - 1 {
        let label = &body[i];
        let out = body[i + 1].as_str()?.to_string();
        // A label may be a single value or an array of values → ezu
        // membership. Normalise both to the ezu filter match value.
        let values = label.clone();
        arms.push((values, out));
        i += 2;
    }
    Some(MatchBuckets {
        key,
        arms,
        fallback,
    })
}
