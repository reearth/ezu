//! Raster and scalar-field utility ops.
//!
//! These nodes operate on rasters and per-pixel scalar fields
//! (`density` is the one exception that consumes feature data — it
//! rasterises points into a field).

mod blend;
mod blur;
mod brightness_contrast;
mod channel_shuffle;
mod circle;
mod color_ramp;
mod color_to_alpha;
mod density;
mod displace;
mod dither;
mod edge_detect;
mod generator_kind;
mod gradient_common;
mod gradient_conic;
mod gradient_diamond;
mod gradient_linear;
mod gradient_radial;
mod hillshade;
mod hsl;
mod invert;
mod levels;
mod map_range;
mod mix;
mod morphology;
mod mosaic;
mod noise;
mod noise_field;
mod palette;
mod place;
mod posterize;
mod quantize;
mod saturate;
mod sharpen;
mod slope;
mod solid;
mod terrain_common;
mod threshold;
mod tiling;
mod warp;
