//! Hex color parsing — the one color literal syntax of the style spec
//! (`#rrggbb` / `#rrggbbaa`). Shared by node factories (literal fields,
//! `$param` defaults) and hosts (CLI / server parameter values).

/// Parse `#rrggbb` / `#rrggbbaa` into straight (non-premultiplied)
/// sRGB components in `[0, 255]`. Returns `None` on malformed input.
pub fn parse_hex_color_u8(s: &str) -> Option<[u8; 4]> {
    let s = s.strip_prefix('#')?;
    let (r, g, b, a) = match s.len() {
        6 => (
            u8::from_str_radix(&s[0..2], 16).ok()?,
            u8::from_str_radix(&s[2..4], 16).ok()?,
            u8::from_str_radix(&s[4..6], 16).ok()?,
            255,
        ),
        8 => (
            u8::from_str_radix(&s[0..2], 16).ok()?,
            u8::from_str_radix(&s[2..4], 16).ok()?,
            u8::from_str_radix(&s[4..6], 16).ok()?,
            u8::from_str_radix(&s[6..8], 16).ok()?,
        ),
        _ => return None,
    };
    Some([r, g, b, a])
}

/// Like [`parse_hex_color_u8`] but with components scaled to `[0, 1]`
/// (still sRGB-encoded, not linearized).
pub fn parse_hex_color(s: &str) -> Option<[f32; 4]> {
    let [r, g, b, a] = parse_hex_color_u8(s)?;
    Some([
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rgb_and_rgba() {
        assert_eq!(parse_hex_color_u8("#ff0080"), Some([255, 0, 128, 255]));
        assert_eq!(parse_hex_color_u8("#ff008040"), Some([255, 0, 128, 64]));
        assert_eq!(parse_hex_color_u8("ff0080"), None);
        assert_eq!(parse_hex_color_u8("#ff00"), None);
        assert_eq!(parse_hex_color_u8("#gg0080"), None);
    }

    #[test]
    fn float_form_scales() {
        let c = parse_hex_color("#ff0000").unwrap();
        assert_eq!(c, [1.0, 0.0, 0.0, 1.0]);
    }
}
