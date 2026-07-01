//! Raster utility ops.
//!
//! These nodes operate purely on rasters and don't consume feature
//! data.

mod blend;
mod blur;
mod brightness_contrast;
mod channel_shuffle;
mod circle;
mod color_ramp;
mod color_to_alpha;
mod displace;
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
mod place;
mod posterize;
mod quantize;
mod sharpen;
mod slope;
mod solid;
mod terrain_common;
mod threshold;
mod tiling;
mod warp;
