//! Geometry ops — `Features -> Features` transforms (centroid, buffer,
//! simplify, …) inspired by turf.js, plus `contour`, which extracts
//! isoline `Features` from a `ScalarField`.

mod bbox;
mod boundary;
mod buffer;
mod centroid;
mod contour;
mod convex_hull;
mod dash;
mod dot_density;
mod feature_boolean;
mod hatch;
mod medial_axis;
mod resample;
mod simplify;
mod transform;
mod triangulate;
mod voronoi;
mod voronoi_fracture;
mod wave;
