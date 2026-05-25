//! Feature source ops — produce `Features` from host-bound asset
//! layers (MVT, GeoJSON, …) or synthesize them from style fields /
//! tile geometry.

mod dem;
mod features;
mod image;
mod literal_geometry;
mod point_grid;
mod tile_bounds;
