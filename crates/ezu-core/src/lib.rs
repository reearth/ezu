//! Core types shared across the ezu workspace.
//!
//! - Tile / world coordinate conversions
//! - Deterministic seeding from world coordinates (for seamless tile rendering)

pub mod coord;
pub mod seed;

pub use coord::{TileId, WorldPos};
pub use seed::world_seed;
