//! Port kinds — the type system of the DAG.

use std::fmt;

/// The kind of value that flows along an edge.
///
/// Every edge connects ports of identical kind; type checks happen
/// during [`Graph`](crate::Graph) construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortKind {
    /// MVT features (geometry + properties), already filtered.
    Features,
    /// RGBA buffer, padded canvas-sized.
    Raster,
    /// hokusai brush handle plus overrides.
    Brush,
    /// Constant value (color, number, bool). Cheap to fan out.
    Scalar,
}

impl fmt::Display for PortKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PortKind::Features => "features",
            PortKind::Raster => "raster",
            PortKind::Brush => "brush",
            PortKind::Scalar => "scalar",
        })
    }
}

/// One declared input port on a node.
#[derive(Debug, Clone)]
pub struct PortSpec {
    /// Name of the port, e.g. `"mask"`, `"base"`. Used as the key in the
    /// style document's per-node JSON object.
    pub name: &'static str,
    /// The kind of value this port expects.
    pub kind: PortKind,
    /// If true, the port may be left unconnected.
    pub optional: bool,
}

impl PortSpec {
    pub const fn new(name: &'static str, kind: PortKind) -> Self {
        Self {
            name,
            kind,
            optional: false,
        }
    }

    pub const fn optional(mut self) -> Self {
        self.optional = true;
        self
    }
}

/// Coordinate-space a node operates in.
///
/// `Tile` outputs depend only on the tile rectangle; `World` outputs are
/// a function of world position and must use deterministic seeding so
/// adjacent tiles agree at borders. `Inherit` adopts the coordinate
/// space of its inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordSpace {
    Tile,
    World,
    Inherit,
}
