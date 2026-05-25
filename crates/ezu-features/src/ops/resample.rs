//! Line-density adjustment: Chaikin corner-smoothing, uniform
//! densification, and arc-length resampling. All operate on the
//! crate's integer-coord polylines and polygon rings.

/// One Chaikin iteration: each interior segment becomes two new
/// vertices at 1/4 and 3/4 along it. Open polylines keep their
/// original endpoints; closed rings wrap around so the smoothing is
/// continuous at the seam.
fn chaikin_once(pts: &[(i32, i32)], closed: bool) -> Vec<(i32, i32)> {
    let n = pts.len();
    if n < 2 {
        return pts.to_vec();
    }
    let mut out: Vec<(i32, i32)> = Vec::with_capacity(n * 2);
    if !closed {
        out.push(pts[0]);
    }
    let count = if closed { n } else { n - 1 };
    for i in 0..count {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        // Q = 3/4 a + 1/4 b ; R = 1/4 a + 3/4 b
        let qx = 0.75 * a.0 as f64 + 0.25 * b.0 as f64;
        let qy = 0.75 * a.1 as f64 + 0.25 * b.1 as f64;
        let rx = 0.25 * a.0 as f64 + 0.75 * b.0 as f64;
        let ry = 0.25 * a.1 as f64 + 0.75 * b.1 as f64;
        out.push((qx.round() as i32, qy.round() as i32));
        out.push((rx.round() as i32, ry.round() as i32));
    }
    if !closed {
        out.push(*pts.last().unwrap());
    }
    out
}

/// Apply Chaikin smoothing `iterations` times. Each iteration roughly
/// doubles the vertex count and rounds corners. Two or three
/// iterations are usually enough for visibly smooth curves without
/// blowing up vertex counts.
pub fn smooth(pts: &[(i32, i32)], iterations: u32, closed: bool) -> Vec<(i32, i32)> {
    let mut cur = pts.to_vec();
    for _ in 0..iterations {
        cur = chaikin_once(&cur, closed);
    }
    cur
}

/// Densify a polyline so no segment is longer than `target_px`.
/// Original vertices are preserved. With `closed = true` the last →
/// first segment is also densified.
pub fn densify(pts: &[(i32, i32)], target_px: f64, closed: bool) -> Vec<(i32, i32)> {
    if pts.len() < 2 || target_px <= 0.0 {
        return pts.to_vec();
    }
    let n = pts.len();
    let count = if closed { n } else { n - 1 };
    let mut out: Vec<(i32, i32)> = Vec::with_capacity(n);
    for i in 0..count {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        out.push(a);
        let dx = (b.0 - a.0) as f64;
        let dy = (b.1 - a.1) as f64;
        let len = (dx * dx + dy * dy).sqrt();
        if len <= target_px {
            continue;
        }
        let steps = (len / target_px).ceil() as usize;
        for k in 1..steps {
            let t = k as f64 / steps as f64;
            let x = a.0 as f64 + dx * t;
            let y = a.1 as f64 + dy * t;
            out.push((x.round() as i32, y.round() as i32));
        }
    }
    if !closed {
        out.push(*pts.last().unwrap());
    }
    out
}

/// Resample a polyline to evenly-spaced points at exactly `spacing_px`
/// apart along arc length. The first vertex is always emitted; later
/// vertices are sampled along segment lengths. For `closed`, the seam
/// is treated as a continuous arc, so the output may not include the
/// original first vertex twice.
pub fn resample(pts: &[(i32, i32)], spacing_px: f64, closed: bool) -> Vec<(i32, i32)> {
    if pts.len() < 2 || spacing_px <= 0.0 {
        return pts.to_vec();
    }
    let n = pts.len();
    let count = if closed { n } else { n - 1 };
    // Total arc length.
    let mut total = 0.0;
    let mut seg_lens: Vec<f64> = Vec::with_capacity(count);
    for i in 0..count {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        let dx = (b.0 - a.0) as f64;
        let dy = (b.1 - a.1) as f64;
        let l = (dx * dx + dy * dy).sqrt();
        seg_lens.push(l);
        total += l;
    }
    if total < spacing_px {
        return pts.to_vec();
    }
    let n_out = (total / spacing_px).round() as usize;
    if n_out < 2 {
        return pts.to_vec();
    }
    let actual_spacing = total / n_out as f64;
    let mut out: Vec<(i32, i32)> = Vec::with_capacity(n_out + 1);
    out.push(pts[0]);
    let mut target = actual_spacing;
    let mut acc = 0.0;
    for i in 0..count {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        let seg_len = seg_lens[i];
        while target <= acc + seg_len && out.len() < n_out {
            let t = (target - acc) / seg_len;
            let x = a.0 as f64 + (b.0 - a.0) as f64 * t;
            let y = a.1 as f64 + (b.1 - a.1) as f64 * t;
            out.push((x.round() as i32, y.round() as i32));
            target += actual_spacing;
        }
        acc += seg_len;
    }
    if !closed {
        let last = *pts.last().unwrap();
        if out.last().copied() != Some(last) {
            out.push(last);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smooth_open_polyline_keeps_endpoints() {
        let pts = vec![(0, 0), (10, 0), (10, 10)];
        let sm = smooth(&pts, 1, false);
        assert_eq!(sm.first(), Some(&(0, 0)));
        assert_eq!(sm.last(), Some(&(10, 10)));
        assert!(sm.len() > pts.len(), "smoothing should add vertices");
    }

    #[test]
    fn densify_inserts_intermediate_points() {
        let pts = vec![(0, 0), (20, 0)];
        let d = densify(&pts, 5.0, false);
        // 20 / 5 = 4 segments → 5 vertices.
        assert_eq!(d.len(), 5);
        assert_eq!(d[0], (0, 0));
        assert_eq!(d[4], (20, 0));
    }

    #[test]
    fn resample_emits_uniform_arclength_points() {
        let pts = vec![(0, 0), (100, 0)];
        let r = resample(&pts, 25.0, false);
        // 100 / 25 = 4 spacings → 5 evenly-spaced points.
        assert_eq!(r.len(), 5);
        assert_eq!(r[0], (0, 0));
        assert_eq!(r[4], (100, 0));
        assert_eq!(r[1], (25, 0));
    }
}
