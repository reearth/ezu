# Test fonts

Subsets of **Noto Sans Regular** (Copyright 2022 The Noto Project
Authors, <https://github.com/notofonts/latin-greek-cyrillic>), licensed
under the SIL Open Font License 1.1 (see `OFL.txt` alongside). Used only
by this crate's `text` feature tests.

Produced with `fonttools subset` from
`fonts/NotoSans/hinted/ttf/NotoSans-Regular.ttf` of
<https://github.com/notofonts/notofonts.github.io>:

- `NotoSans-Regular.latin.ttf` — U+0020–U+002F, U+003A–U+007E,
  U+00C0–U+00FF, U+0300–U+0301 (ASCII minus digits, Latin-1 letters,
  combining grave/acute), keeping `kern`/`liga`/`ccmp`/`mark`/`mkmk`.
- `NotoSans-Regular.digits.ttf` — U+0030–U+0039, U+0300–U+0301 (digits
  plus the same combining marks), keeping `kern`/`ccmp`/`mark`/`mkmk`.

The two subsets have deliberately disjoint letter/digit coverage so the
fallback-itemization tests can observe which font a char resolved to.
