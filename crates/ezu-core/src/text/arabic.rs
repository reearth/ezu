//! Arabic joining for the SDF backend.
//!
//! A MapLibre glyph stack is one pre-rendered bitmap per codepoint with
//! no `GSUB` table, so an Arabic run drawn from one cannot be shaped the
//! way an outline font is. What is available instead is the Unicode
//! **presentation forms** (U+FB50‥U+FDFF, U+FE70‥U+FEFF): a separate
//! codepoint for each of a letter's isolated / final / initial / medial
//! shapes, which a glyphs endpoint serves like any other range. Picking
//! the right one per letter and looking *that* up in the stack is how
//! MapLibre gets joined Arabic out of a glyph stack too.
//!
//! Two letters join when the earlier one can join on its left and the
//! later one on its right (Unicode joining types D/C and R/D/C), with
//! transparent chars — the vowel and i'jam marks — skipped over. That
//! rule and this module's table are the whole of it; the table is
//! generated from the presentation forms' own decomposition mappings,
//! so it covers the Persian and Urdu letters as well as Arabic's.
//!
//! # Scope
//!
//! - Only the lam-alef ligatures are formed. They are the pair Arabic
//!   requires be ligated; the optional lam-jeem-family ligatures are
//!   left as their component letters, as MapLibre leaves them.
//! - A letter with no presentation form (the Arabic Extended blocks
//!   have several) is treated as joining to neither side, so it draws
//!   unjoined and its neighbours take their unjoined shapes. The
//!   outline backend shapes all of them properly.

/// A letter's shape, by which sides it joins on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Form {
    Isolated,
    Final,
    Initial,
    Medial,
}

impl Form {
    /// The shape of a letter joined on the given sides, `previous` and
    /// `next` in logical order.
    fn of(previous: bool, next: bool) -> Form {
        match (previous, next) {
            (true, true) => Form::Medial,
            (true, false) => Form::Final,
            (false, true) => Form::Initial,
            (false, false) => Form::Isolated,
        }
    }

    /// The shape to try when this one has no codepoint: a letter that
    /// joins on one side at least keeps that side's simpler shape.
    fn relaxed(self) -> Option<Form> {
        match self {
            Form::Medial => Some(Form::Final),
            Form::Initial => Some(Form::Isolated),
            Form::Final | Form::Isolated => None,
        }
    }

    fn index(self) -> usize {
        match self {
            Form::Isolated => 0,
            Form::Final => 1,
            Form::Initial => 2,
            Form::Medial => 3,
        }
    }
}

/// Base letter → its `[isolated, final, initial, medial]` presentation
/// forms, `0` where the letter has none. Sorted by base for lookup.
///
/// Generated from the Unicode character database: every presentation
/// form whose decomposition is one of those four tags over a single
/// base char.
#[rustfmt::skip]
const FORMS: &[(u16, [u16; 4])] = &[
    (0x0621, [0xFE80, 0x0000, 0x0000, 0x0000]), // Hamza
    (0x0622, [0xFE81, 0xFE82, 0x0000, 0x0000]), // Alef With Madda Above
    (0x0623, [0xFE83, 0xFE84, 0x0000, 0x0000]), // Alef With Hamza Above
    (0x0624, [0xFE85, 0xFE86, 0x0000, 0x0000]), // Waw With Hamza Above
    (0x0625, [0xFE87, 0xFE88, 0x0000, 0x0000]), // Alef With Hamza Below
    (0x0626, [0xFE89, 0xFE8A, 0xFE8B, 0xFE8C]), // Yeh With Hamza Above
    (0x0627, [0xFE8D, 0xFE8E, 0x0000, 0x0000]), // Alef
    (0x0628, [0xFE8F, 0xFE90, 0xFE91, 0xFE92]), // Beh
    (0x0629, [0xFE93, 0xFE94, 0x0000, 0x0000]), // Teh Marbuta
    (0x062A, [0xFE95, 0xFE96, 0xFE97, 0xFE98]), // Teh
    (0x062B, [0xFE99, 0xFE9A, 0xFE9B, 0xFE9C]), // Theh
    (0x062C, [0xFE9D, 0xFE9E, 0xFE9F, 0xFEA0]), // Jeem
    (0x062D, [0xFEA1, 0xFEA2, 0xFEA3, 0xFEA4]), // Hah
    (0x062E, [0xFEA5, 0xFEA6, 0xFEA7, 0xFEA8]), // Khah
    (0x062F, [0xFEA9, 0xFEAA, 0x0000, 0x0000]), // Dal
    (0x0630, [0xFEAB, 0xFEAC, 0x0000, 0x0000]), // Thal
    (0x0631, [0xFEAD, 0xFEAE, 0x0000, 0x0000]), // Reh
    (0x0632, [0xFEAF, 0xFEB0, 0x0000, 0x0000]), // Zain
    (0x0633, [0xFEB1, 0xFEB2, 0xFEB3, 0xFEB4]), // Seen
    (0x0634, [0xFEB5, 0xFEB6, 0xFEB7, 0xFEB8]), // Sheen
    (0x0635, [0xFEB9, 0xFEBA, 0xFEBB, 0xFEBC]), // Sad
    (0x0636, [0xFEBD, 0xFEBE, 0xFEBF, 0xFEC0]), // Dad
    (0x0637, [0xFEC1, 0xFEC2, 0xFEC3, 0xFEC4]), // Tah
    (0x0638, [0xFEC5, 0xFEC6, 0xFEC7, 0xFEC8]), // Zah
    (0x0639, [0xFEC9, 0xFECA, 0xFECB, 0xFECC]), // Ain
    (0x063A, [0xFECD, 0xFECE, 0xFECF, 0xFED0]), // Ghain
    (0x0641, [0xFED1, 0xFED2, 0xFED3, 0xFED4]), // Feh
    (0x0642, [0xFED5, 0xFED6, 0xFED7, 0xFED8]), // Qaf
    (0x0643, [0xFED9, 0xFEDA, 0xFEDB, 0xFEDC]), // Kaf
    (0x0644, [0xFEDD, 0xFEDE, 0xFEDF, 0xFEE0]), // Lam
    (0x0645, [0xFEE1, 0xFEE2, 0xFEE3, 0xFEE4]), // Meem
    (0x0646, [0xFEE5, 0xFEE6, 0xFEE7, 0xFEE8]), // Noon
    (0x0647, [0xFEE9, 0xFEEA, 0xFEEB, 0xFEEC]), // Heh
    (0x0648, [0xFEED, 0xFEEE, 0x0000, 0x0000]), // Waw
    (0x0649, [0xFEEF, 0xFEF0, 0xFBE8, 0xFBE9]), // Alef Maksura
    (0x064A, [0xFEF1, 0xFEF2, 0xFEF3, 0xFEF4]), // Yeh
    (0x0671, [0xFB50, 0xFB51, 0x0000, 0x0000]), // Alef Wasla
    (0x0677, [0xFBDD, 0x0000, 0x0000, 0x0000]), // U With Hamza Above
    (0x0679, [0xFB66, 0xFB67, 0xFB68, 0xFB69]), // Tteh
    (0x067A, [0xFB5E, 0xFB5F, 0xFB60, 0xFB61]), // Tteheh
    (0x067B, [0xFB52, 0xFB53, 0xFB54, 0xFB55]), // Beeh
    (0x067E, [0xFB56, 0xFB57, 0xFB58, 0xFB59]), // Peh
    (0x067F, [0xFB62, 0xFB63, 0xFB64, 0xFB65]), // Teheh
    (0x0680, [0xFB5A, 0xFB5B, 0xFB5C, 0xFB5D]), // Beheh
    (0x0683, [0xFB76, 0xFB77, 0xFB78, 0xFB79]), // Nyeh
    (0x0684, [0xFB72, 0xFB73, 0xFB74, 0xFB75]), // Dyeh
    (0x0686, [0xFB7A, 0xFB7B, 0xFB7C, 0xFB7D]), // Tcheh
    (0x0687, [0xFB7E, 0xFB7F, 0xFB80, 0xFB81]), // Tcheheh
    (0x0688, [0xFB88, 0xFB89, 0x0000, 0x0000]), // Ddal
    (0x068C, [0xFB84, 0xFB85, 0x0000, 0x0000]), // Dahal
    (0x068D, [0xFB82, 0xFB83, 0x0000, 0x0000]), // Ddahal
    (0x068E, [0xFB86, 0xFB87, 0x0000, 0x0000]), // Dul
    (0x0691, [0xFB8C, 0xFB8D, 0x0000, 0x0000]), // Rreh
    (0x0698, [0xFB8A, 0xFB8B, 0x0000, 0x0000]), // Jeh
    (0x06A4, [0xFB6A, 0xFB6B, 0xFB6C, 0xFB6D]), // Veh
    (0x06A6, [0xFB6E, 0xFB6F, 0xFB70, 0xFB71]), // Peheh
    (0x06A9, [0xFB8E, 0xFB8F, 0xFB90, 0xFB91]), // Keheh
    (0x06AD, [0xFBD3, 0xFBD4, 0xFBD5, 0xFBD6]), // Ng
    (0x06AF, [0xFB92, 0xFB93, 0xFB94, 0xFB95]), // Gaf
    (0x06B1, [0xFB9A, 0xFB9B, 0xFB9C, 0xFB9D]), // Ngoeh
    (0x06B3, [0xFB96, 0xFB97, 0xFB98, 0xFB99]), // Gueh
    (0x06BA, [0xFB9E, 0xFB9F, 0x0000, 0x0000]), // Noon Ghunna
    (0x06BB, [0xFBA0, 0xFBA1, 0xFBA2, 0xFBA3]), // Rnoon
    (0x06BE, [0xFBAA, 0xFBAB, 0xFBAC, 0xFBAD]), // Heh Doachashmee
    (0x06C0, [0xFBA4, 0xFBA5, 0x0000, 0x0000]), // Heh With Yeh Above
    (0x06C1, [0xFBA6, 0xFBA7, 0xFBA8, 0xFBA9]), // Heh Goal
    (0x06C5, [0xFBE0, 0xFBE1, 0x0000, 0x0000]), // Kirghiz Oe
    (0x06C6, [0xFBD9, 0xFBDA, 0x0000, 0x0000]), // Oe
    (0x06C7, [0xFBD7, 0xFBD8, 0x0000, 0x0000]), // U
    (0x06C8, [0xFBDB, 0xFBDC, 0x0000, 0x0000]), // Yu
    (0x06C9, [0xFBE2, 0xFBE3, 0x0000, 0x0000]), // Kirghiz Yu
    (0x06CB, [0xFBDE, 0xFBDF, 0x0000, 0x0000]), // Ve
    (0x06CC, [0xFBFC, 0xFBFD, 0xFBFE, 0xFBFF]), // Farsi Yeh
    (0x06D0, [0xFBE4, 0xFBE5, 0xFBE6, 0xFBE7]), // E
    (0x06D2, [0xFBAE, 0xFBAF, 0x0000, 0x0000]), // Yeh Barree
    (0x06D3, [0xFBB0, 0xFBB1, 0x0000, 0x0000]), // Yeh Barree With Hamza Above
];

/// The lam-alef ligatures: the alef that follows a lam, and the
/// `[isolated, final]` codepoints of the pair. Lam never joins on its
/// right in these, so there is no initial or medial form.
const LAM_ALEF: &[(u16, [u16; 2])] = &[
    (0x0622, [0xFEF5, 0xFEF6]), // Lam + Alef With Madda Above
    (0x0623, [0xFEF7, 0xFEF8]), // Lam + Alef With Hamza Above
    (0x0625, [0xFEF9, 0xFEFA]), // Lam + Alef With Hamza Below
    (0x0627, [0xFEFB, 0xFEFC]), // Lam + Alef
];

const LAM: char = '\u{0644}';
const TATWEEL: char = '\u{0640}';
const ZWJ: char = '\u{200D}';

/// How a char joins to its neighbours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Joining {
    /// Joins on neither side, and breaks the join between its
    /// neighbours (any non-Arabic char, and ZWNJ).
    None,
    /// Joins only to the char before it (alef, dal, reh, waw, …).
    Right,
    /// Joins on both sides (beh, lam, seen, …).
    Dual,
    /// Joins on both sides but has no shape of its own (tatweel, ZWJ).
    Causing,
    /// Skipped over when neighbours look for each other (the vowel and
    /// i'jam marks).
    Transparent,
}

impl Joining {
    /// Whether a char of this type can join to the char after it.
    fn to_next(self) -> bool {
        matches!(self, Joining::Dual | Joining::Causing)
    }

    /// Whether a char of this type can join to the char before it.
    fn to_previous(self) -> bool {
        matches!(self, Joining::Right | Joining::Dual | Joining::Causing)
    }
}

/// The presentation forms of `c`, if it has any.
fn forms_of(c: char) -> Option<&'static [u16; 4]> {
    let cp = u16::try_from(c as u32).ok()?;
    let ix = FORMS.binary_search_by_key(&cp, |(base, _)| *base).ok()?;
    Some(&FORMS[ix].1)
}

/// How `c` joins to its neighbours.
pub(crate) fn joining(c: char) -> Joining {
    if is_transparent(c) {
        return Joining::Transparent;
    }
    if c == TATWEEL || c == ZWJ {
        return Joining::Causing;
    }
    match forms_of(c) {
        // A letter with an initial or medial form joins on its left.
        Some(f) if f[Form::Initial.index()] != 0 || f[Form::Medial.index()] != 0 => Joining::Dual,
        // One with a final form joins only on its right. One with
        // nothing but an isolated form (hamza) joins on neither, and
        // must not pull its neighbours into joined shapes.
        Some(f) if f[Form::Final.index()] != 0 => Joining::Right,
        _ => Joining::None,
    }
}

/// Whether `c` is transparent to joining — a mark that sits over or
/// under its base letter, which two letters look past to find each
/// other. The Arabic blocks' marks plus the variation selectors and
/// combining half marks that can follow them.
pub(crate) fn is_transparent(c: char) -> bool {
    matches!(
        c as u32,
        0x0610..=0x061A      // Arabic honorifics
        | 0x064B..=0x065F    // harakat and other marks
        | 0x0670             // superscript alef
        | 0x06D6..=0x06DC    // small high marks
        | 0x06DF..=0x06E4
        | 0x06E7..=0x06E8
        | 0x06EA..=0x06ED
        | 0x0898..=0x089F    // Arabic Extended-B marks
        | 0x08CA..=0x08FF    // Arabic Extended-A marks
        | 0xFE00..=0xFE0F    // variation selectors
        | 0xFE20..=0xFE2F // combining half marks
    )
}

/// Whether the char before `c` joins to it and whether the char after
/// does, given its nearest non-transparent neighbours in logical order.
pub(crate) fn joined_sides(previous: Option<char>, c: char, next: Option<char>) -> (bool, bool) {
    let own = joining(c);
    let from_previous = own.to_previous() && previous.is_some_and(|p| joining(p).to_next());
    let to_next = own.to_next() && next.is_some_and(|n| joining(n).to_previous());
    (from_previous, to_next)
}

/// Append the presentation form of `c` in the given joining context to
/// `out`, followed by the simpler shapes to fall back to when the glyph
/// stack does not carry it — most specific first.
///
/// Appends nothing for anything that is not a joining Arabic letter,
/// which is the answer for the overwhelming majority of chars. `out` is
/// the caller's scratch buffer so a label of them allocates nothing.
pub(crate) fn shaped_forms(c: char, from_previous: bool, to_next: bool, out: &mut Vec<char>) {
    let Some(forms) = forms_of(c) else {
        return;
    };
    let mut form = Some(Form::of(from_previous, to_next));
    while let Some(f) = form {
        if forms[f.index()] != 0 {
            out.extend(char::from_u32(u32::from(forms[f.index()])));
        }
        form = f.relaxed();
    }
}

/// The lam-alef ligature for a lam followed by `alef`, in the given
/// joining context — `from_previous` being whether the lam itself joins
/// to the char before it.
pub(crate) fn lam_alef(alef: char, from_previous: bool) -> Option<char> {
    let cp = u16::try_from(alef as u32).ok()?;
    let ix = LAM_ALEF.binary_search_by_key(&cp, |(a, _)| *a).ok()?;
    let pair = LAM_ALEF[ix].1;
    char::from_u32(u32::from(pair[usize::from(from_previous)]))
}

/// Whether `c` is a lam, the only letter that ligates here.
pub(crate) fn is_lam(c: char) -> bool {
    c == LAM
}

/// Every presentation form `c` could be drawn as, in any joining
/// context — the lam-alef ligatures included, for a lam and for the
/// four alefs one can ligate with.
///
/// Empty for anything that is not an Arabic letter. A host that must
/// bind glyphs before a label can be shaped needs these codepoints, not
/// just the letters themselves, since the letters are only what the
/// label was written with.
pub fn presentation_forms(c: char) -> Vec<char> {
    let own = forms_of(c).into_iter().flatten().copied();
    let ligatures = LAM_ALEF
        .iter()
        .filter(move |(alef, _)| c == LAM || u32::from(*alef) == c as u32)
        .flat_map(|(_, pair)| pair.iter().copied());
    own.chain(ligatures)
        .filter(|&cp| cp != 0)
        .filter_map(|cp| char::from_u32(u32::from(cp)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every form in the table is a real BMP codepoint in one of the
    /// two presentation-form blocks, and the bases are sorted.
    #[test]
    fn the_table_is_well_formed() {
        assert!(FORMS.windows(2).all(|w| w[0].0 < w[1].0));
        for (base, forms) in FORMS {
            assert!(forms.iter().any(|&f| f != 0), "{base:#06X} has no form");
            for &f in forms.iter().filter(|&&f| f != 0) {
                assert!(
                    (0xFB50..=0xFDFF).contains(&f) || (0xFE70..=0xFEFF).contains(&f),
                    "{f:#06X} is outside the presentation-form blocks"
                );
            }
        }
    }

    #[test]
    fn hamza_joins_on_neither_side() {
        // It has an isolated form and nothing else, so a letter before
        // it must not be pulled into an initial shape.
        assert_eq!(joining('\u{0621}'), Joining::None);
        assert_eq!(joining('\u{0627}'), Joining::Right);
        assert_eq!(joining('\u{0644}'), Joining::Dual);
        assert_eq!(joining('a'), Joining::None);
    }

    #[test]
    fn a_word_takes_the_shapes_its_joins_call_for() {
        // الورود — alef and lam start it, so the lam is initial and the
        // waw after it final; the letters after a right-joining one are
        // isolated again.
        let word: Vec<char> = "الورود".chars().collect();
        let shaped: Vec<char> = word
            .iter()
            .enumerate()
            .map(|(i, &c)| {
                let (prev, next) = joined_sides(
                    i.checked_sub(1).map(|p| word[p]),
                    c,
                    word.get(i + 1).copied(),
                );
                let mut forms = Vec::new();
                shaped_forms(c, prev, next, &mut forms);
                forms[0]
            })
            .collect();
        assert_eq!(
            shaped,
            vec![
                '\u{FE8D}', // alef isolated
                '\u{FEDF}', // lam initial
                '\u{FEEE}', // waw final
                '\u{FEAD}', // reh isolated
                '\u{FEED}', // waw isolated
                '\u{FEA9}', // dal isolated
            ]
        );
    }

    #[test]
    fn a_mark_does_not_break_a_join() {
        // beh + fatha + teh: the mark is transparent, so beh and teh
        // still see each other.
        let (prev, next) = joined_sides(None, '\u{0628}', Some('\u{062A}'));
        assert_eq!((prev, next), (false, true));
        assert!(is_transparent('\u{064E}'));
    }

    #[test]
    fn every_shape_a_letter_can_take_is_listed() {
        // Waw has two forms and no ligature.
        assert_eq!(presentation_forms('\u{0648}'), vec!['\u{FEED}', '\u{FEEE}']);
        // Lam has four, plus both shapes of each lam-alef pair.
        assert_eq!(presentation_forms('\u{0644}').len(), 4 + 2 * LAM_ALEF.len());
        // An alef carries its own pair's two shapes as well as its own.
        assert_eq!(
            presentation_forms('\u{0627}'),
            vec!['\u{FE8D}', '\u{FE8E}', '\u{FEFB}', '\u{FEFC}']
        );
        assert!(presentation_forms('a').is_empty());
    }

    #[test]
    fn lam_alef_ligates_in_both_its_shapes() {
        assert_eq!(lam_alef('\u{0627}', false), Some('\u{FEFB}'));
        assert_eq!(lam_alef('\u{0627}', true), Some('\u{FEFC}'));
        assert_eq!(lam_alef('\u{0628}', false), None);
    }

    #[test]
    fn a_shape_the_stack_lacks_relaxes_to_a_simpler_one() {
        // Waw has no medial form, so a waw joined on both sides offers
        // its final one next.
        let mut waw = Vec::new();
        shaped_forms('\u{0648}', true, true, &mut waw);
        assert_eq!(
            waw,
            vec!['\u{FEEE}'],
            "waw only ever has isolated and final"
        );
        let mut beh = Vec::new();
        shaped_forms('\u{0628}', false, true, &mut beh);
        assert_eq!(
            beh,
            vec!['\u{FE91}', '\u{FE8F}'],
            "beh offers its initial form, then its isolated one"
        );
    }
}
