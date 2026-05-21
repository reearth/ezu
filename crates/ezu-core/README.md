# ezu-core

Core types shared across the [`ezu`](../../README.md) workspace.

## Contents

| Module | What it provides |
|---|---|
| `coord` | [`TileId`] (z/x/y) and [`WorldPos`] in the `[0,1]²` Web-Mercator unit square; `tile_to_world()` |
| `seed`  | `world_seed(WorldPos, salt) -> u64` (xxh3) — the foundation of seamless tile boundaries |

World coordinates here are normalized to the unit square so that
zoom-aware jitter and seed derivation stay zoom-stable: the same physical
location always produces the same seed regardless of which zoom level a
particular tile happens to live at.

## Example

```rust
use ezu_core::{seed::world_seed, TileId, WorldPos};

let tile = TileId::new(13, 7276, 3225);
let pos = ezu_core::coord::tile_to_world(tile, 2048.0, 2048.0, 4096.0);
let seed = world_seed(pos, 0xE2_70_DA_B5);
```

See the main [README](../../README.md) for the full project overview.

## License

MIT or Apache-2.0, at your option.
