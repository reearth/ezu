//! Phase 0 paint benches: per-primitive cost breakdown + Brush::clone
//! micro-bench. Synthetic inputs so the bench is reproducible without
//! network / PMTiles access.

use criterion::{criterion_group, criterion_main, Criterion};
use ezu_core::TileId;
use ezu_features::Polygon;
use ezu_paint::{
    paint_lines, paint_polygons, paint_polygons_dabs, Canvas, DabFillStyle, LineStrokeStyle,
    RgbaF32, WatercolorStyle,
};
use tiny_skia::Color;

const TILE_SIZE: u32 = 512;
const PAD: u32 = 24;
const EXTENT: u32 = 4096;
const TILE: TileId = TileId {
    z: 13,
    x: 7276,
    y: 3225,
};

/// Build a grid of rectangular polygons across the MVT extent — stands
/// in for an "earth" or "landuse" layer with a handful of patches.
fn synth_polygons(count_per_axis: u32) -> Vec<Polygon> {
    let step = EXTENT as i32 / count_per_axis as i32;
    let inset = step / 8;
    let mut out = Vec::with_capacity((count_per_axis * count_per_axis) as usize);
    for iy in 0..count_per_axis as i32 {
        for ix in 0..count_per_axis as i32 {
            let x0 = ix * step + inset;
            let y0 = iy * step + inset;
            let x1 = (ix + 1) * step - inset;
            let y1 = (iy + 1) * step - inset;
            out.push(Polygon {
                exterior: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)],
                holes: vec![],
            });
        }
    }
    out
}

/// Build a set of horizontal-ish polylines across the extent — stands
/// in for a "roads" layer.
fn synth_lines(count: u32, points_per_line: u32) -> Vec<Vec<(i32, i32)>> {
    let step_y = EXTENT as i32 / (count as i32 + 1);
    let step_x = EXTENT as i32 / points_per_line as i32;
    (1..=count as i32)
        .map(|iy| {
            let y = iy * step_y;
            (0..points_per_line as i32)
                .map(|ix| {
                    // Light zigzag so dabs cover varied area.
                    let jitter = if ix % 2 == 0 { 0 } else { step_y / 4 };
                    (ix * step_x, y + jitter)
                })
                .collect()
        })
        .collect()
}

fn fixture_brush() -> hokusai::Brush {
    let json = std::fs::read_to_string("../../assets/brushes/watercolor_glazing.myb").expect(
        "bench needs assets/brushes/watercolor_glazing.myb (run from repo root or crate dir)",
    );
    hokusai::myb::from_str(&json).expect("parse watercolor_glazing.myb")
}

// ---------------------------------------------------------------------------

fn bench_paint_polygons(c: &mut Criterion) {
    let polys = synth_polygons(4); // 16 patches
    let style = WatercolorStyle {
        fill: Color::from_rgba8(232, 217, 176, 255),
        edge: Some(Color::from_rgba8(80, 110, 150, 220)),
        edge_width: 1.5,
        blur_sigma: 1.2,
    };
    c.bench_function("paint_polygons (16 patches, blur σ=1.2)", |b| {
        b.iter(|| {
            let mut canvas = Canvas::new_padded(TILE_SIZE, TILE_SIZE, PAD);
            paint_polygons(&mut canvas, &polys, EXTENT, &style);
            std::hint::black_box(canvas);
        });
    });
}

fn bench_paint_polygons_dabs(c: &mut Criterion) {
    let polys = synth_polygons(2); // 4 large water-style patches
    let style = DabFillStyle {
        color: RgbaF32::new(0.34, 0.46, 0.62, 1.0),
        opacity: 0.22,
        radius_px: 7.0,
        hardness: 0.5,
        paint: 1.0,
        spacing_px: 3.0,
        position_jitter: 0.9,
        size_jitter: 0.4,
        opacity_jitter: 0.3,
        value_jitter: 0.08,
    };
    c.bench_function("paint_polygons_dabs (4 patches, r=7 spacing=3)", |b| {
        b.iter(|| {
            let mut canvas = Canvas::new_padded(TILE_SIZE, TILE_SIZE, PAD);
            paint_polygons_dabs(&mut canvas, &polys, EXTENT, TILE, &style);
            std::hint::black_box(canvas);
        });
    });
}

fn bench_paint_lines(c: &mut Criterion) {
    let lines = synth_lines(12, 16);
    let brush = fixture_brush();
    let style = LineStrokeStyle::default();
    c.bench_function("paint_lines serial (12 polylines × 16 vertices)", |b| {
        b.iter(|| {
            let mut canvas = Canvas::new_padded(TILE_SIZE, TILE_SIZE, PAD);
            paint_lines(&mut canvas, &lines, EXTENT, TILE, &brush, &style);
            std::hint::black_box(canvas);
        });
    });

    #[cfg(feature = "parallel")]
    {
        use ezu_paint::paint_lines_parallel;
        c.bench_function(
            "paint_lines parallel (12 polylines × 16 vertices, Rayon)",
            |b| {
                b.iter(|| {
                    let mut canvas = Canvas::new_padded(TILE_SIZE, TILE_SIZE, PAD);
                    paint_lines_parallel(&mut canvas, &lines, EXTENT, TILE, &brush, &style);
                    std::hint::black_box(canvas);
                });
            },
        );
    }
}

fn bench_brush_clone(c: &mut Criterion) {
    let brush = fixture_brush();
    c.bench_function("Brush::clone (watercolor_glazing)", |b| {
        b.iter(|| {
            let cloned = brush.clone();
            std::hint::black_box(cloned);
        });
    });
}

criterion_group!(
    paint_benches,
    bench_paint_polygons,
    bench_paint_polygons_dabs,
    bench_paint_lines,
    bench_brush_clone,
);
criterion_main!(paint_benches);
