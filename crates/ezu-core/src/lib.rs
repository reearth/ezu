//! Core types shared across the ezu workspace.
//!
//! - Tile / world coordinate conversions
//! - Print-frame geometry: zoom, tile size and tile range for a map at a
//!   given scale and paper size
//! - Deterministic seeding from world coordinates (for seamless tile rendering)
//! - Text shaping / layout / drawing (feature `text`)

pub mod color;
pub mod coord;
pub mod frame;
pub mod seed;
#[cfg(feature = "text")]
pub mod text;

pub use color::{interpolate as interpolate_color, InterpSpace};
pub use coord::{TileId, WorldPos};
pub use frame::{Frame, FrameError, FrameRequest};
pub use seed::world_seed;
