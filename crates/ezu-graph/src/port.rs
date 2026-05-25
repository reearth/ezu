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
    /// RGBA buffer matching the canvas's `padded_size`. Every node
    /// that outputs `Raster` MUST produce a `padded_size × padded_size`
    /// buffer so the host's final crop and other consumers' size
    /// assumptions hold.
    Raster,
    /// RGBA buffer at the asset's native dimensions, NOT canvas-sized.
    /// Used as the "raw image" carrier for sprite / texture sources
    /// (`image`) whose output is consumed by placement ops
    /// (`stamp`, `tiling`, `place`) that decide how to map them onto
    /// the canvas. Sprites cannot be wired directly into raster
    /// transforms or the document `output` — those expect `Raster`.
    Sprite,
    /// hokusai brush handle plus overrides.
    Brush,
    /// Constant value (color, number, bool). Cheap to fan out.
    Scalar,
    /// Per-pixel single-channel `f32` grid matching the canvas's
    /// `padded_size`. The general carrier for floating-point grids:
    /// elevation (with `GeoScale` populated, produced by `dem`),
    /// signed distance, scalar noise, slope angle. Consumed by
    /// terrain ops (`hillshade`, `slope`, `hypsometric`) and any
    /// future scalar→raster mapper (`color-ramp`).
    ScalarField,
}

impl fmt::Display for PortKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PortKind::Features => "features",
            PortKind::Raster => "raster",
            PortKind::Sprite => "sprite",
            PortKind::Brush => "brush",
            PortKind::Scalar => "scalar",
            PortKind::ScalarField => "scalar-field",
        })
    }
}

/// One declared input port on a node.
///
/// `accepts` lists the [`PortKind`]s this port will accept. A polymorphic
/// port lists multiple kinds (e.g. `&[Raster, Sprite]`); a strict port
/// lists exactly one. The graph builder checks the upstream node's
/// resolved output kind against this list.
#[derive(Debug, Clone)]
pub struct PortSpec {
    /// Name of the port, e.g. `"mask"`, `"base"`. Used as the key in the
    /// style document's per-node JSON object.
    pub name: &'static str,
    /// The kinds of value this port will accept. Non-empty.
    pub accepts: &'static [PortKind],
    /// If true, the port may be left unconnected.
    pub optional: bool,
}

impl PortSpec {
    /// Strict port — accepts exactly one kind. For polymorphic ports,
    /// build the struct directly or use [`PortSpec::any_of`].
    pub const fn new(name: &'static str, accepts: &'static [PortKind]) -> Self {
        Self {
            name,
            accepts,
            optional: false,
        }
    }

    /// Polymorphic port — accepts any of the listed kinds.
    pub const fn any_of(name: &'static str, accepts: &'static [PortKind]) -> Self {
        Self {
            name,
            accepts,
            optional: false,
        }
    }

    pub const fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// True if this port would accept a value of the given kind.
    pub fn accepts_kind(&self, kind: PortKind) -> bool {
        self.accepts.iter().any(|k| *k == kind)
    }

    /// The first kind in the accepts list — convenient for ports that
    /// are de-facto monomorphic (most are).
    pub fn primary_kind(&self) -> PortKind {
        self.accepts[0]
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
