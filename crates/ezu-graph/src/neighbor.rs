//! The reserved binding-name convention for **neighbour** tile data.
//!
//! Per-tile asset bindings are named `<source>` (scalar fields, rasters)
//! or `<source>.<layer>` (feature layers) — see [`AssetLoader`] docs.
//! Cross-tile features (label collision) need the 3×3 neighbourhood, so
//! a neighbour's copy of a per-tile binding is addressed by suffixing the
//! plain name with `@<dx>,<dy>`, where `dx`/`dy` ∈ `-1..=1` are the tile
//! offsets (`+x` east, `+y` south, matching MVT tile numbering). `@0,0`
//! is never emitted — the plain name always stays the tile's own data.
//!
//! Example: with a `features` source `roads` and layer `road`, the tile's
//! own layer binds under `roads.road`; the eastern neighbour's copy binds
//! under `roads.road@1,0`. A node that gathers neighbour candidates lists
//! the eight offset names in [`Node::asset_inputs`], and a host that can
//! fetch neighbour tiles binds them under exactly these names. A host that
//! binds only the centre tile simply leaves the neighbour names unbound;
//! consumers degrade to centre-only gracefully.
//!
//! [`AssetLoader`]: crate::eval::AssetLoader
//! [`Node::asset_inputs`]: crate::node::Node::asset_inputs

/// Format the neighbour binding name for `base` at offset `(dx, dy)`.
/// `(0, 0)` yields the plain `base` unchanged (the tile's own data).
pub fn neighbor_binding(base: &str, dx: i32, dy: i32) -> String {
    if dx == 0 && dy == 0 {
        base.to_string()
    } else {
        format!("{base}@{dx},{dy}")
    }
}

/// The eight neighbour binding names for `base`, in a fixed order
/// (row-major, `dy` then `dx`, skipping `(0, 0)`).
pub fn neighbor_bindings(base: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(8);
    for dy in -1..=1 {
        for dx in -1..=1 {
            if dx == 0 && dy == 0 {
                continue;
            }
            out.push(neighbor_binding(base, dx, dy));
        }
    }
    out
}

/// Split a binding name into `(base, (dx, dy))`. A name without the
/// `@<dx>,<dy>` suffix is the tile's own data → offset `(0, 0)`. A
/// malformed suffix is treated as part of the base name (offset `(0, 0)`)
/// rather than an error, so unrelated names containing `@` pass through.
pub fn parse_neighbor_binding(name: &str) -> (&str, i32, i32) {
    let Some((base, off)) = name.rsplit_once('@') else {
        return (name, 0, 0);
    };
    let Some((sx, sy)) = off.split_once(',') else {
        return (name, 0, 0);
    };
    match (sx.parse::<i32>(), sy.parse::<i32>()) {
        (Ok(dx), Ok(dy)) if (-1..=1).contains(&dx) && (-1..=1).contains(&dy) => (base, dx, dy),
        _ => (name, 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        assert_eq!(neighbor_binding("s.l", 0, 0), "s.l");
        assert_eq!(neighbor_binding("s.l", 1, 0), "s.l@1,0");
        assert_eq!(neighbor_binding("s.l", -1, 1), "s.l@-1,1");
        for dy in -1..=1 {
            for dx in -1..=1 {
                let name = neighbor_binding("roads.road", dx, dy);
                assert_eq!(parse_neighbor_binding(&name), ("roads.road", dx, dy));
            }
        }
    }

    #[test]
    fn eight_neighbors() {
        let names = neighbor_bindings("s.l");
        assert_eq!(names.len(), 8);
        assert!(!names.iter().any(|n| n.ends_with("@0,0")));
        // Deterministic order.
        assert_eq!(names[0], "s.l@-1,-1");
        assert_eq!(names[7], "s.l@1,1");
    }

    #[test]
    fn plain_and_malformed_pass_through() {
        assert_eq!(parse_neighbor_binding("s.l"), ("s.l", 0, 0));
        // Out-of-range or non-numeric suffix stays part of the base.
        assert_eq!(parse_neighbor_binding("s.l@2,0"), ("s.l@2,0", 0, 0));
        assert_eq!(parse_neighbor_binding("s.l@x,y"), ("s.l@x,y", 0, 0));
        // A data: URL carries `@`-free but scheme-y text; untouched.
        assert_eq!(
            parse_neighbor_binding("http://h/{z}"),
            ("http://h/{z}", 0, 0)
        );
    }
}
