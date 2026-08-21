//! Comments in a style document.
//!
//! A style is a long, mostly declarative file, and the interesting part
//! of a node is rarely what it does — it is why the author chose that
//! width, that colour, that zoom cutoff. JSON has nowhere to put that,
//! so styles carry `//` line comments and `/* … */` block comments and
//! this module removes them before the JSON parser sees the text.
//!
//! Removal means *blanking*: each comment byte becomes a space, and
//! newlines stay where they are. Nothing moves, so the line and column
//! in a parse error still point at the author's file rather than at a
//! shortened copy of it. Comments are stripped on the way in and the
//! file is never rewritten, so nothing can drop them.
//!
//! Only comments are added to JSON here — trailing commas and unquoted
//! keys stay errors, so a style remains close enough to JSON that
//! ordinary tooling can still read it once the comments are gone.

use std::borrow::Cow;

/// Blank every comment in `src`, leaving all other bytes at their
/// original offsets. Returns the input untouched when it holds no
/// comments.
///
/// `//` and `/*` inside a JSON string are text, not comments — a URL
/// like `"https://example.com/{z}/{x}/{y}"` comes through whole.
pub fn blank_comments(src: &str) -> Result<Cow<'_, str>, CommentError> {
    let bytes = src.as_bytes();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => i = skip_string(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                let start = i;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                spans.push((start, i));
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let start = i;
                i += 2;
                loop {
                    if i + 1 >= bytes.len() {
                        return Err(CommentError {
                            line: line_of(bytes, start),
                        });
                    }
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                spans.push((start, i));
            }
            _ => i += 1,
        }
    }
    if spans.is_empty() {
        return Ok(Cow::Borrowed(src));
    }
    let mut out = src.as_bytes().to_vec();
    for (start, end) in spans {
        for b in &mut out[start..end] {
            // Newlines and carriage returns survive so every following
            // line keeps its number.
            if *b != b'\n' && *b != b'\r' {
                *b = b' ';
            }
        }
    }
    // Only ASCII bytes were replaced, and only with ASCII, so whatever
    // was valid UTF-8 still is.
    Ok(Cow::Owned(
        String::from_utf8(out).expect("blanking preserves UTF-8"),
    ))
}

/// A block comment that never closed. Reported on its own rather than
/// left to the JSON parser, which would blame whatever the runaway
/// comment swallowed.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("unterminated block comment opened on line {line}")]
pub struct CommentError {
    pub line: usize,
}

/// Index just past the string literal starting at `open` (the opening
/// quote). An unterminated string is left to the JSON parser, which
/// already words that error well.
fn skip_string(bytes: &[u8], open: usize) -> usize {
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            // Skip whatever follows a backslash, so `\"` and `\\` do not
            // look like the end of the string.
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    i
}

fn line_of(bytes: &[u8], offset: usize) -> usize {
    1 + bytes[..offset].iter().filter(|&&b| b == b'\n').count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Blank `src`, checking the invariants that hold for every input:
    /// nothing moves, and the JSON that comes out is the JSON the author
    /// meant. Returns the blanked text for the caller to inspect.
    fn blank_to(src: &str, want_json: &str) -> String {
        let out = blank_comments(src).expect("no comment error").into_owned();
        assert_eq!(src.len(), out.len(), "byte length changed");
        let newlines = |s: &str| s.bytes().filter(|&b| b == b'\n').count();
        assert_eq!(newlines(src), newlines(&out), "newline count changed");
        let got: serde_json::Value = serde_json::from_str(&out).expect("blanked text parses");
        let want: serde_json::Value = serde_json::from_str(want_json).expect("expectation parses");
        assert_eq!(got, want);
        out
    }

    #[test]
    fn line_comments_go_but_their_lines_stay() {
        let out = blank_to("{\n  // the base coat\n  \"a\": 1\n}", r#"{"a": 1}"#);
        assert!(!out.contains("base coat"));
        assert_eq!(out.lines().nth(1).unwrap().trim(), "");
    }

    #[test]
    fn block_comments_go_inline_and_across_lines() {
        blank_to("[1, /* two */ 3]", "[1, 3]");
        let out = blank_to(
            "{\n/* why\n   this\n   width */\n\"a\": 1\n}",
            r#"{"a": 1}"#,
        );
        assert!(!out.contains("width"));
    }

    #[test]
    fn comment_openers_inside_strings_are_text() {
        // The case that matters most in practice: tile URL templates.
        let src = r#"{"url": "https://example.com/{z}/{x}/{y}.pbf", "b": "/* not a comment */"}"#;
        assert_eq!(blank_comments(src).unwrap(), src);
    }

    #[test]
    fn escapes_do_not_end_a_string_early() {
        // The `\"` must not read as the closing quote, or the `//` after
        // it would look like a comment.
        let src = r#"{"a": "say \"hi\" // here", "b": 1}"#;
        assert_eq!(blank_comments(src).unwrap(), src);
        // An escaped backslash does end the string, so what follows is a
        // comment.
        let out = blank_to("{\"a\": \"back\\\\\" // gone\n}", r#"{"a": "back\\"}"#);
        assert!(!out.contains("gone"));
    }

    #[test]
    fn a_comment_may_end_the_file() {
        blank_to("{} // done", "{}");
        blank_to("{} /* done */", "{}");
    }

    #[test]
    fn non_ascii_comment_text_survives_blanking() {
        let out = blank_to("{\n  // 幅は 1.2 px\n  \"a\": 1\n}", r#"{"a": 1}"#);
        assert!(!out.contains('幅'));
    }

    #[test]
    fn crlf_line_endings_are_preserved() {
        let out = blank_to("{\r\n  // note\r\n  \"a\": 1\r\n}", r#"{"a": 1}"#);
        assert_eq!(out.matches("\r\n").count(), 3);
    }

    #[test]
    fn a_document_without_comments_is_not_copied() {
        let src = r#"{"a": 1}"#;
        assert!(matches!(blank_comments(src).unwrap(), Cow::Borrowed(_)));
    }

    #[test]
    fn unterminated_block_comment_names_its_line() {
        let err = blank_comments("{\n\"a\": 1\n/* and then nothing").unwrap_err();
        assert_eq!(err.line, 3);
    }
}
