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
    if s.eq_ignore_ascii_case("transparent") {
        return Some(("#000000".to_string(), 0.0));
    }
    if let Some(hex) = named_color(s) {
        return parse_hex(hex.strip_prefix('#').unwrap_or(hex));
    }
    None
}

/// CSS/SVG named colours → `#rrggbb`. Case-insensitive. MapLibre accepts
/// these anywhere a colour is expected.
fn named_color(s: &str) -> Option<&'static str> {
    Some(match s.to_ascii_lowercase().as_str() {
        "aliceblue" => "#f0f8ff",
        "antiquewhite" => "#faebd7",
        "aqua" => "#00ffff",
        "aquamarine" => "#7fffd4",
        "azure" => "#f0ffff",
        "beige" => "#f5f5dc",
        "bisque" => "#ffe4c4",
        "black" => "#000000",
        "blanchedalmond" => "#ffebcd",
        "blue" => "#0000ff",
        "blueviolet" => "#8a2be2",
        "brown" => "#a52a2a",
        "burlywood" => "#deb887",
        "cadetblue" => "#5f9ea0",
        "chartreuse" => "#7fff00",
        "chocolate" => "#d2691e",
        "coral" => "#ff7f50",
        "cornflowerblue" => "#6495ed",
        "cornsilk" => "#fff8dc",
        "crimson" => "#dc143c",
        "cyan" => "#00ffff",
        "darkblue" => "#00008b",
        "darkcyan" => "#008b8b",
        "darkgoldenrod" => "#b8860b",
        "darkgray" | "darkgrey" => "#a9a9a9",
        "darkgreen" => "#006400",
        "darkkhaki" => "#bdb76b",
        "darkmagenta" => "#8b008b",
        "darkolivegreen" => "#556b2f",
        "darkorange" => "#ff8c00",
        "darkorchid" => "#9932cc",
        "darkred" => "#8b0000",
        "darksalmon" => "#e9967a",
        "darkseagreen" => "#8fbc8f",
        "darkslateblue" => "#483d8b",
        "darkslategray" | "darkslategrey" => "#2f4f4f",
        "darkturquoise" => "#00ced1",
        "darkviolet" => "#9400d3",
        "deeppink" => "#ff1493",
        "deepskyblue" => "#00bfff",
        "dimgray" | "dimgrey" => "#696969",
        "dodgerblue" => "#1e90ff",
        "firebrick" => "#b22222",
        "floralwhite" => "#fffaf0",
        "forestgreen" => "#228b22",
        "fuchsia" => "#ff00ff",
        "gainsboro" => "#dcdcdc",
        "ghostwhite" => "#f8f8ff",
        "gold" => "#ffd700",
        "goldenrod" => "#daa520",
        "gray" | "grey" => "#808080",
        "green" => "#008000",
        "greenyellow" => "#adff2f",
        "honeydew" => "#f0fff0",
        "hotpink" => "#ff69b4",
        "indianred" => "#cd5c5c",
        "indigo" => "#4b0082",
        "ivory" => "#fffff0",
        "khaki" => "#f0e68c",
        "lavender" => "#e6e6fa",
        "lavenderblush" => "#fff0f5",
        "lawngreen" => "#7cfc00",
        "lemonchiffon" => "#fffacd",
        "lightblue" => "#add8e6",
        "lightcoral" => "#f08080",
        "lightcyan" => "#e0ffff",
        "lightgoldenrodyellow" => "#fafad2",
        "lightgray" | "lightgrey" => "#d3d3d3",
        "lightgreen" => "#90ee90",
        "lightpink" => "#ffb6c1",
        "lightsalmon" => "#ffa07a",
        "lightseagreen" => "#20b2aa",
        "lightskyblue" => "#87cefa",
        "lightslategray" | "lightslategrey" => "#778899",
        "lightsteelblue" => "#b0c4de",
        "lightyellow" => "#ffffe0",
        "lime" => "#00ff00",
        "limegreen" => "#32cd32",
        "linen" => "#faf0e6",
        "magenta" => "#ff00ff",
        "maroon" => "#800000",
        "mediumaquamarine" => "#66cdaa",
        "mediumblue" => "#0000cd",
        "mediumorchid" => "#ba55d3",
        "mediumpurple" => "#9370db",
        "mediumseagreen" => "#3cb371",
        "mediumslateblue" => "#7b68ee",
        "mediumspringgreen" => "#00fa9a",
        "mediumturquoise" => "#48d1cc",
        "mediumvioletred" => "#c71585",
        "midnightblue" => "#191970",
        "mintcream" => "#f5fffa",
        "mistyrose" => "#ffe4e1",
        "moccasin" => "#ffe4b5",
        "navajowhite" => "#ffdead",
        "navy" => "#000080",
        "oldlace" => "#fdf5e6",
        "olive" => "#808000",
        "olivedrab" => "#6b8e23",
        "orange" => "#ffa500",
        "orangered" => "#ff4500",
        "orchid" => "#da70d6",
        "palegoldenrod" => "#eee8aa",
        "palegreen" => "#98fb98",
        "paleturquoise" => "#afeeee",
        "palevioletred" => "#db7093",
        "papayawhip" => "#ffefd5",
        "peachpuff" => "#ffdab9",
        "peru" => "#cd853f",
        "pink" => "#ffc0cb",
        "plum" => "#dda0dd",
        "powderblue" => "#b0e0e6",
        "purple" => "#800080",
        "rebeccapurple" => "#663399",
        "red" => "#ff0000",
        "rosybrown" => "#bc8f8f",
        "royalblue" => "#4169e1",
        "saddlebrown" => "#8b4513",
        "salmon" => "#fa8072",
        "sandybrown" => "#f4a460",
        "seagreen" => "#2e8b57",
        "seashell" => "#fff5ee",
        "sienna" => "#a0522d",
        "silver" => "#c0c0c0",
        "skyblue" => "#87ceeb",
        "slateblue" => "#6a5acd",
        "slategray" | "slategrey" => "#708090",
        "snow" => "#fffafa",
        "springgreen" => "#00ff7f",
        "steelblue" => "#4682b4",
        "tan" => "#d2b48c",
        "teal" => "#008080",
        "thistle" => "#d8bfd8",
        "tomato" => "#ff6347",
        "turquoise" => "#40e0d0",
        "violet" => "#ee82ee",
        "wheat" => "#f5deb3",
        "white" => "#ffffff",
        "whitesmoke" => "#f5f5f5",
        "yellow" => "#ffff00",
        "yellowgreen" => "#9acd32",
        _ => return None,
    })
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
