//! Feature source ops — produce `Features` from host-bound asset
//! layers (MVT, GeoJSON, …) or synthesize them from style fields /
//! tile geometry.

mod dem;
mod features;
mod graticule;
mod icon;
mod image;
mod literal_geometry;
mod point_grid;
mod point_scatter;
mod raster;
mod tile_bounds;
