//! Core types shared across the ezu workspace.
//!
//! - Tile / world coordinate conversions
//! - Deterministic seeding from world coordinates (for seamless tile rendering)
//! - Text shaping / layout / drawing (feature `text`)

pub mod color;
pub mod coord;
pub mod seed;
#[cfg(feature = "text")]
pub mod text;

pub use color::{interpolate as interpolate_color, InterpSpace};
pub use coord::{TileId, WorldPos};
pub use seed::world_seed;
