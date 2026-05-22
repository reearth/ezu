//! Built-in brushes, bundled into the binary via `include_str!`.
//!
//! All bundled brushes come from [`mypaint/mypaint-brushes`] (CC0 1.0
//! Universal). Authors and the upstream paths are listed in
//! [`builtin/CREDITS.md`]. The brushes are exposed to the renderer
//! through [`BrushBankLoader::new`] — every `BrushBankLoader` starts
//! pre-populated with the names below, so a style can reference them
//! without the host having to ship `.myb` files of its own.
//!
//! [`mypaint/mypaint-brushes`]: https://github.com/mypaint/mypaint-brushes
//! [`builtin/CREDITS.md`]: https://github.com/reearth/ezu/blob/main/crates/ezu-paint/src/builtin/CREDITS.md
//! [`BrushBankLoader::new`]: crate::host::BrushBankLoader::new

/// `(name, myb-json)` pairs for every brush bundled with `ezu-paint`.
///
/// Names match the `src` strings used by the reference styles in
/// `crates/ezu/examples/styles/`.
pub const BUILTIN_BRUSHES: &[(&str, &str)] = &[
    (
        "watercolor_glazing",
        include_str!("builtin/watercolor_glazing.myb"),
    ),
    (
        "watercolor_expressive",
        include_str!("builtin/watercolor_expressive.myb"),
    ),
    (
        "large_watercolor_fringe",
        include_str!("builtin/large_watercolor_fringe.myb"),
    ),
    (
        "only_water_fringe",
        include_str!("builtin/only_water_fringe.myb"),
    ),
    ("2B_pencil", include_str!("builtin/2B_pencil.myb")),
    ("4H_pencil", include_str!("builtin/4H_pencil.myb")),
];
