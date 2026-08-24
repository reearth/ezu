//! Bidirectional resolution (UAX #9), the two pieces the text pipeline
//! needs from it.
//!
//! [`levels`] resolves the embedding level of every char of a label,
//! which itemization splits runs on (so a right-to-left run reaches the
//! shaper flagged as such) and which line reordering reads.
//!
//! [`reorder_visual`] is rule L2: turn one line's logical sequence into
//! the order it is painted in, left to right. It runs per line and after
//! line breaking, as in the reference implementations — a wrapped label
//! reorders each line independently, and the break itself is chosen on
//! the logical sequence, where the choice is order-independent anyway.
//!
//! Explicit isolates and overrides (U+2066‥U+2069, U+202A‥U+202E) are
//! handled by the algorithm proper; nothing here special-cases them.

use unicode_bidi::BidiInfo;

/// The embedding level of each char of `text`, in logical order.
///
/// Paragraph direction is auto-detected per paragraph from its first
/// strong char (UAX #9 P2/P3), so a `\n`-separated label resolves each
/// line's paragraph on its own — matching how the label was authored
/// rather than forcing one direction on the whole string.
pub(crate) fn levels(text: &str) -> Vec<u8> {
    let info = BidiInfo::new(text, None);
    text.char_indices()
        .map(|(byte, _)| info.levels[byte].number())
        .collect()
}

/// Reorder one line's `items` from logical into visual order in place
/// (UAX #9 rule L2), `level_of` giving each item's embedding level.
///
/// Returns with `items` untouched when the line holds no right-to-left
/// level, so the overwhelmingly common all-LTR label pays one pass over
/// the levels and nothing else.
pub(crate) fn reorder_visual<T>(items: &mut [T], level_of: impl Fn(&T) -> u8) {
    let levels: Vec<u8> = items.iter().map(level_of).collect();
    let (Some(&max), Some(lowest_odd)) = (
        levels.iter().max(),
        levels.iter().copied().filter(|l| l % 2 == 1).min(),
    ) else {
        return;
    };
    // From the highest level down to the lowest odd one, reverse every
    // maximal stretch at that level or above. `levels` is deliberately
    // not permuted along with `items`: each reversal only ever moves
    // items whose level is at or above the current one, so the "level ≥
    // n" predicate at every later (lower) n is unchanged by it.
    let mut level = max;
    while level >= lowest_odd {
        let mut i = 0;
        while i < levels.len() {
            if levels[i] < level {
                i += 1;
                continue;
            }
            let start = i;
            while i < levels.len() && levels[i] >= level {
                i += 1;
            }
            items[start..i].reverse();
        }
        level -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_even_for_latin_and_odd_for_hebrew() {
        assert!(levels("abc").iter().all(|l| l.is_multiple_of(2)));
        assert!(levels("תל אביב").iter().all(|l| !l.is_multiple_of(2)));
    }

    #[test]
    fn a_paragraph_resolves_its_own_direction() {
        // Two lines of one string: the Latin one stays LTR even though a
        // right-to-left paragraph precedes it.
        let text = "תל\nabc";
        let lv = levels(text);
        let chars: Vec<char> = text.chars().collect();
        let latin = chars.iter().position(|&c| c == 'a').unwrap();
        assert!(!lv[0].is_multiple_of(2), "the Hebrew paragraph is RTL");
        assert!(lv[latin].is_multiple_of(2), "the Latin paragraph is LTR");
    }

    #[test]
    fn reorder_reverses_a_right_to_left_run() {
        let mut items = vec![0, 1, 2, 3];
        reorder_visual(&mut items, |_| 1);
        assert_eq!(items, vec![3, 2, 1, 0]);
    }

    #[test]
    fn reorder_leaves_a_left_to_right_line_alone() {
        let mut items = vec![0, 1, 2, 3];
        reorder_visual(&mut items, |_| 0);
        assert_eq!(items, vec![0, 1, 2, 3]);
    }

    #[test]
    fn a_number_inside_a_right_to_left_run_keeps_its_digit_order() {
        // "RTL RTL 1 2 RTL" — the Arabic-numeral run sits at level 2, so
        // the line reverses around it while its digits stay in order.
        let levels = [1u8, 1, 2, 2, 1];
        let mut items = vec![0usize, 1, 2, 3, 4];
        reorder_visual(&mut items, |&i| levels[i]);
        assert_eq!(items, vec![4, 2, 3, 1, 0]);
    }
}
