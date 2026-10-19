//! Print-frame geometry: which tiles, at what zoom and tile size, render a
//! map at a given scale and paper size.
//!
//! A printed map is asked for as "1:1000 at 200 dpi, 245 × 174 mm, centred
//! here", not as a zoom level. A tile renderer only draws whole tiles at
//! integer zooms, so the scale has to be reached through the tile size
//! instead: at zoom `z`, a tile of `T` px spans `1 / 2^z` of the world, and
//! choosing `T` sets how many ground metres one output pixel covers. This
//! module picks `z` and `T`, says how close the integer `T` lands to the
//! requested scale, and locates the frame on the resulting pixel grid so a
//! host can render the covered tiles and crop them to the frame exactly.
//!
//! Everything here is plain arithmetic on Web Mercator; nothing is fetched
//! or rendered.

use std::fmt;
use std::ops::Range;

use crate::coord::{lat_to_world_y, lon_to_world_x, EARTH_CIRCUMFERENCE_M, MERCATOR_MAX_LAT};

/// Deepest zoom [`Frame::compute`] considers when picking one itself.
/// Tile pyramids rarely go past this, and the tile size it would need for
/// any print scale is already far below a usable one.
pub const MAX_ZOOM: u8 = 24;

/// Smallest tile size a frame will render at. Below it a tile is mostly
/// padding, and a style's per-tile work dominates the render.
pub const MIN_TILE_SIZE: u32 = 16;

/// Largest tile size a frame will render at. Above it a single tile's
/// padded canvas, and every intermediate raster the graph keeps for it,
/// grows into hundreds of megabytes.
pub const MAX_TILE_SIZE: u32 = 8192;

/// Metres in an inch, for turning dots per inch into metres per pixel.
const METRES_PER_INCH: f64 = 0.0254;

/// What a print frame is asked for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameRequest {
    /// Longitude of the frame's centre, in degrees (WGS84).
    pub center_lon: f64,
    /// Latitude of the frame's centre, in degrees (WGS84). The scale is
    /// matched here; Web Mercator stretches it away from this latitude.
    pub center_lat: f64,
    /// Scale denominator: `1000` for 1:1000.
    pub scale: f64,
    /// Output resolution, in dots per inch. Together with `scale` it fixes
    /// how many ground metres one output pixel covers.
    pub dpi: f64,
    /// Output width in pixels. A caller working in millimetres converts
    /// with `round(mm / 25.4 * dpi)`.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    /// Render at this zoom, deriving the tile size from it. `None` picks
    /// the zoom whose tile size is nearest `style_tile_size`.
    pub zoom: Option<u8>,
    /// The style's own `tile-size`. A style is tuned for its tile size
    /// (zoom-dependent widths, label density), so the default zoom keeps
    /// the rendered tile size as close to it as the scale allows.
    pub style_tile_size: u32,
}

/// Where a print frame sits on the tile grid it renders from.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// Zoom the tiles render at.
    pub zoom: u8,
    /// Tile size, in pixels, the tiles render at. Rounded to a whole
    /// pixel, so the scale is met only as closely as [`Frame::scale`] says.
    pub tile_size: u32,
    /// The scale denominator this zoom and tile size actually produce at
    /// the centre latitude.
    pub scale: f64,
    /// Output size in pixels, as requested.
    pub width: u32,
    pub height: u32,
    /// Top-left corner of the frame in world pixels — the pixel grid of the
    /// whole world at this zoom and tile size, `tile_size · 2^zoom` wide.
    /// Rounded to a whole pixel so tiles land on the frame without
    /// resampling.
    pub left: i64,
    pub top: i64,
    /// How far rounding the corner moved the frame, in pixels: the
    /// requested centre sits at `(width / 2 + shift.0, height / 2 + shift.1)`
    /// in the output image. Each component is within ±0.5.
    pub shift: (f64, f64),
    /// Tile columns the frame touches, as an exclusive range.
    pub tiles_x: Range<u32>,
    /// Tile rows the frame touches, as an exclusive range. Clamped to the
    /// world, so a frame reaching past the Mercator edge leaves that part
    /// of the image empty.
    pub tiles_y: Range<u32>,
}

/// Why a frame could not be computed.
#[derive(Debug, Clone, PartialEq)]
pub enum FrameError {
    /// A request field is out of its domain (non-positive scale, latitude
    /// past the Mercator edge, zero-sized output, ...).
    Invalid(String),
    /// The scale needs a tile size outside
    /// [`MIN_TILE_SIZE`]..=[`MAX_TILE_SIZE`] at the zoom given or chosen.
    TileSize { zoom: u8, tile_size: f64 },
    /// The frame crosses the antimeridian, which the tile range cannot
    /// express as one run of columns.
    Antimeridian,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::Invalid(msg) => f.write_str(msg),
            FrameError::TileSize { zoom, tile_size } => {
                let hint = if *tile_size < MIN_TILE_SIZE as f64 {
                    "try a lower zoom"
                } else {
                    "try a higher zoom"
                };
                write!(
                    f,
                    "this scale needs a tile size of {tile_size:.1} px at zoom {zoom}, \
                     outside {MIN_TILE_SIZE}..={MAX_TILE_SIZE}; {hint}"
                )
            }
            FrameError::Antimeridian => {
                f.write_str("the frame crosses the antimeridian, which is not supported")
            }
        }
    }
}

impl std::error::Error for FrameError {}

/// Ground metres one pixel covers at latitude `lat_deg` for tiles of
/// `tile_size` px at zoom `zoom`.
pub fn metres_per_pixel(lat_deg: f64, tile_size: f64, zoom: u8) -> f64 {
    EARTH_CIRCUMFERENCE_M * libm::cos(lat_deg.to_radians()) / (tile_size * zoom_factor(zoom))
}

/// `2^zoom`, exactly.
fn zoom_factor(zoom: u8) -> f64 {
    (1u64 << zoom) as f64
}

impl Frame {
    /// Work out the zoom, tile size and pixel placement for a request.
    pub fn compute(req: &FrameRequest) -> Result<Frame, FrameError> {
        validate(req)?;
        let wanted_mpp = req.scale * METRES_PER_INCH / req.dpi;
        // The unrounded tile size that meets the scale at `zoom`.
        let exact_tile = |zoom: u8| metres_per_pixel(req.center_lat, 1.0, zoom) / wanted_mpp;

        let zoom = match req.zoom {
            Some(z) => {
                if z > 31 {
                    return Err(FrameError::Invalid(format!(
                        "zoom {z} is past the deepest a tile id can address (31)"
                    )));
                }
                z
            }
            None => {
                // Each zoom halves the tile size, so compare in log space:
                // the zoom whose tile size is the smallest ratio away from
                // the style's. Ascending, keeping the first on a tie, so a
                // tie goes to the lower zoom.
                let target = req.style_tile_size as f64;
                let mut best = 0u8;
                let mut best_dist = f64::INFINITY;
                for z in 0..=MAX_ZOOM {
                    let dist = libm::log(exact_tile(z) / target).abs();
                    if dist < best_dist {
                        best = z;
                        best_dist = dist;
                    }
                }
                best
            }
        };

        let exact = exact_tile(zoom);
        let rounded = exact.round();
        if !(MIN_TILE_SIZE as f64..=MAX_TILE_SIZE as f64).contains(&rounded) {
            return Err(FrameError::TileSize {
                zoom,
                tile_size: exact,
            });
        }
        let tile_size = rounded as u32;
        let scale =
            metres_per_pixel(req.center_lat, tile_size as f64, zoom) * req.dpi / METRES_PER_INCH;

        let world = tile_size as f64 * zoom_factor(zoom);
        let cx = lon_to_world_x(req.center_lon) * world;
        let cy = lat_to_world_y(req.center_lat) * world;
        let exact_left = cx - req.width as f64 / 2.0;
        let exact_top = cy - req.height as f64 / 2.0;
        let left = exact_left.round() as i64;
        let top = exact_top.round() as i64;
        let shift = (exact_left - left as f64, exact_top - top as f64);

        let world_px = tile_size as i64 * (1i64 << zoom);
        if left < 0 || left + req.width as i64 > world_px {
            return Err(FrameError::Antimeridian);
        }

        let t = tile_size as i64;
        let tiles = 1i64 << zoom;
        let span = |start: i64, len: u32| {
            let first = start.div_euclid(t).clamp(0, tiles);
            // Ceiling of the far edge: the last tile any frame pixel lands in.
            let end = (start + len as i64 + t - 1).div_euclid(t).clamp(0, tiles);
            first as u32..end as u32
        };

        Ok(Frame {
            zoom,
            tile_size,
            scale,
            width: req.width,
            height: req.height,
            left,
            top,
            shift,
            tiles_x: span(left, req.width),
            tiles_y: span(top, req.height),
        })
    }

    /// Where tile `(x, y)`'s top-left corner lands in the output image, in
    /// pixels. Negative or past the frame for a tile that only partly
    /// overlaps it.
    pub fn tile_offset(&self, x: u32, y: u32) -> (i64, i64) {
        let t = self.tile_size as i64;
        (x as i64 * t - self.left, y as i64 * t - self.top)
    }

    /// Number of tiles the frame renders.
    pub fn tile_count(&self) -> u64 {
        self.tiles_x.len() as u64 * self.tiles_y.len() as u64
    }
}

fn validate(req: &FrameRequest) -> Result<(), FrameError> {
    let invalid = |msg: String| Err(FrameError::Invalid(msg));
    if !(req.scale.is_finite() && req.scale > 0.0) {
        return invalid(format!(
            "scale must be a positive number, got {}",
            req.scale
        ));
    }
    if !(req.dpi.is_finite() && req.dpi > 0.0) {
        return invalid(format!("dpi must be a positive number, got {}", req.dpi));
    }
    if !(-180.0..=180.0).contains(&req.center_lon) {
        return invalid(format!(
            "centre longitude {} is outside -180..=180",
            req.center_lon
        ));
    }
    if !(-MERCATOR_MAX_LAT..=MERCATOR_MAX_LAT).contains(&req.center_lat) {
        return invalid(format!(
            "centre latitude {} is outside the Web Mercator range ±{MERCATOR_MAX_LAT:.4}",
            req.center_lat
        ));
    }
    if req.width == 0 || req.height == 0 {
        return invalid(format!(
            "frame size must be at least 1 px each way, got {}x{}",
            req.width, req.height
        ));
    }
    if req.zoom.is_none() && req.style_tile_size == 0 {
        return invalid("style tile size must be at least 1 px".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shibuya, Tokyo.
    const LON: f64 = 139.70;
    const LAT: f64 = 35.68;

    fn request(scale: f64, zoom: Option<u8>) -> FrameRequest {
        FrameRequest {
            center_lon: LON,
            center_lat: LAT,
            scale,
            dpi: 200.0,
            // 245 x 174 mm at 200 dpi.
            width: 1929,
            height: 1370,
            zoom,
            style_tile_size: 512,
        }
    }

    #[test]
    fn realised_scale_is_within_a_thousandth_of_the_request() {
        for scale in [500.0, 1000.0, 2500.0] {
            let f = Frame::compute(&request(scale, None)).unwrap();
            let err = (f.scale - scale).abs() / scale;
            assert!(err < 1e-3, "1:{scale} came out 1:{} ({err})", f.scale);
        }
    }

    #[test]
    fn default_zoom_picks_the_tile_size_nearest_the_style() {
        for scale in [500.0, 1000.0, 2500.0] {
            let req = request(scale, None);
            let f = Frame::compute(&req).unwrap();
            let ratio = |tile: f64| libm::log(tile / req.style_tile_size as f64).abs();
            let here = ratio(f.tile_size as f64);
            // One zoom either way doubles or halves the tile size; neither
            // may land closer to the style's.
            assert!(here <= ratio(f.tile_size as f64 * 2.0), "1:{scale}");
            assert!(here <= ratio(f.tile_size as f64 / 2.0), "1:{scale}");
            assert!(here <= std::f64::consts::LN_2 / 2.0 + 1e-3, "1:{scale}");
        }
    }

    #[test]
    fn default_zoom_compares_tile_sizes_by_ratio() {
        // At zoom 18 this scale needs tiles of `exact` px, at 19 half that.
        // A style tile size of 0.73 · exact is nearer the half in pixels
        // but nearer `exact` as a ratio (1.37× vs 1.46×), so the ratio
        // has to win: zoom 18.
        let exact = metres_per_pixel(LAT, 1.0, 18) / (1000.0 * 0.0254 / 200.0);
        let f = Frame::compute(&FrameRequest {
            style_tile_size: (exact * 0.73).round() as u32,
            ..request(1000.0, None)
        })
        .unwrap();
        assert_eq!(f.zoom, 18);
        // Below the ratio midpoint (exact / √2 ≈ 0.707 · exact) it flips.
        let f = Frame::compute(&FrameRequest {
            style_tile_size: (exact * 0.69).round() as u32,
            ..request(1000.0, None)
        })
        .unwrap();
        assert_eq!(f.zoom, 19);
    }

    #[test]
    fn fixed_zoom_back_computes_the_tile_size() {
        let f = Frame::compute(&request(1000.0, Some(19))).unwrap();
        assert_eq!(f.zoom, 19);
        let one_up = Frame::compute(&request(1000.0, Some(18))).unwrap();
        // One zoom shallower needs tiles twice the size for the same scale.
        assert!((one_up.tile_size as i64 - 2 * f.tile_size as i64).abs() <= 1);
        assert!((f.scale - 1000.0).abs() / 1000.0 < 1e-2);
    }

    #[test]
    fn fixed_zoom_with_an_absurd_tile_size_is_refused() {
        match Frame::compute(&request(1000.0, Some(24))) {
            Err(FrameError::TileSize {
                zoom: 24,
                tile_size,
            }) => {
                assert!(tile_size < MIN_TILE_SIZE as f64)
            }
            other => panic!("expected a tile-size error, got {other:?}"),
        }
        let err = Frame::compute(&request(1000.0, Some(8))).unwrap_err();
        assert!(matches!(err, FrameError::TileSize { zoom: 8, .. }));
        assert!(err.to_string().contains("higher zoom"), "{err}");
    }

    #[test]
    fn tile_range_covers_exactly_the_frame() {
        for scale in [500.0, 1000.0, 2500.0] {
            let f = Frame::compute(&request(scale, None)).unwrap();
            let t = f.tile_size as i64;
            let (x0, y0) = f.tile_offset(f.tiles_x.start, f.tiles_y.start);
            let (x1, y1) = f.tile_offset(f.tiles_x.end, f.tiles_y.end);
            // The first tile starts at or before the frame's corner, and
            // not a whole tile before it.
            assert!(x0 <= 0 && x0 > -t, "1:{scale} x0={x0}");
            assert!(y0 <= 0 && y0 > -t, "1:{scale} y0={y0}");
            // The last tile ends at or past the far edge, and not a whole
            // tile past it.
            assert!(x1 >= f.width as i64 && x1 < f.width as i64 + t, "1:{scale}");
            assert!(
                y1 >= f.height as i64 && y1 < f.height as i64 + t,
                "1:{scale}"
            );
        }
    }

    #[test]
    fn a_frame_on_tile_edges_takes_no_extra_tile() {
        // Centre exactly on a tile corner at zoom 1 with T = 256, frame of
        // two tiles each way: the four tiles around the corner, no more.
        let exact_scale = metres_per_pixel(0.0, 256.0, 1) * 96.0 / 0.0254;
        let f = Frame::compute(&FrameRequest {
            center_lon: 0.0,
            center_lat: 0.0,
            scale: exact_scale,
            dpi: 96.0,
            width: 512,
            height: 512,
            zoom: Some(1),
            style_tile_size: 256,
        })
        .unwrap();
        assert_eq!(f.tile_size, 256);
        assert_eq!((f.left, f.top), (0, 0));
        assert_eq!(f.tiles_x, 0..2);
        assert_eq!(f.tiles_y, 0..2);
    }

    #[test]
    fn rounding_moves_the_frame_by_at_most_half_a_pixel() {
        for scale in [500.0, 1000.0, 2500.0] {
            for (w, h) in [(1929, 1370), (1000, 1001), (7, 3)] {
                let f = Frame::compute(&FrameRequest {
                    width: w,
                    height: h,
                    ..request(scale, None)
                })
                .unwrap();
                assert!(f.shift.0.abs() <= 0.5, "{:?}", f.shift);
                assert!(f.shift.1.abs() <= 0.5, "{:?}", f.shift);
                // The centre, put back, is where the projection puts it.
                let world = f.tile_size as f64 * (1u64 << f.zoom) as f64;
                let cx = f.left as f64 + w as f64 / 2.0 + f.shift.0;
                let cy = f.top as f64 + h as f64 / 2.0 + f.shift.1;
                assert!((cx - lon_to_world_x(LON) * world).abs() < 1e-6);
                assert!((cy - lat_to_world_y(LAT) * world).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn a_frame_across_the_antimeridian_is_refused() {
        let req = FrameRequest {
            center_lon: 179.9999,
            ..request(1000.0, None)
        };
        assert_eq!(Frame::compute(&req), Err(FrameError::Antimeridian));
    }

    #[test]
    fn nonsense_requests_are_refused() {
        for req in [
            FrameRequest {
                scale: 0.0,
                ..request(1000.0, None)
            },
            FrameRequest {
                dpi: -1.0,
                ..request(1000.0, None)
            },
            FrameRequest {
                center_lat: 89.0,
                ..request(1000.0, None)
            },
            FrameRequest {
                width: 0,
                ..request(1000.0, None)
            },
        ] {
            assert!(
                matches!(Frame::compute(&req), Err(FrameError::Invalid(_))),
                "{req:?}"
            );
        }
    }
}
