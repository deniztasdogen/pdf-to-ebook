//! Everything downstream works in **PDF points with a top-left origin**.
//!
//! The two sources disagree: pdfium reports points with a bottom-left origin,
//! tesseract reports pixels with a top-left origin. Both are converted here, at
//! the boundary, so no layout code ever has to care which path it came from.
//!
//! Points are kept rather than normalising to 0..1 on purpose. Every threshold
//! in the layout code is derived from per-page statistics (median font size,
//! column width), so normalising would buy nothing — and dividing x by width
//! while dividing y by height would silently make horizontal and vertical
//! measurements incomparable.

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Rect {
            x0: x0.min(x1),
            y0: y0.min(y1),
            x1: x0.max(x1),
            y1: y0.max(y1),
        }
    }

    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    pub fn center_x(&self) -> f32 {
        (self.x0 + self.x1) / 2.0
    }

    pub fn union(&self, o: &Rect) -> Rect {
        Rect {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }

    pub fn union_all<'a>(mut it: impl Iterator<Item = &'a Rect>) -> Option<Rect> {
        let first = *it.next()?;
        Some(it.fold(first, |a, b| a.union(b)))
    }

    /// Horizontal overlap in points. Negative means a gap.
    pub fn x_overlap(&self, o: &Rect) -> f32 {
        self.x1.min(o.x1) - self.x0.max(o.x0)
    }
}

/// Median of an unsorted slice. Returns 0.0 for an empty slice so callers can
/// use the result in a threshold without special-casing.
pub fn median(vals: &[f32]) -> f32 {
    if vals.is_empty() {
        return 0.0;
    }
    let mut v: Vec<f32> = vals.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Median absolute deviation, used to spot outlier glyph heights (drop caps)
/// without letting them drag the median around.
pub fn mad(vals: &[f32], med: f32) -> f32 {
    if vals.is_empty() {
        return 0.0;
    }
    let devs: Vec<f32> = vals.iter().map(|v| (v - med).abs()).collect();
    median(&devs)
}

/// The most common value, to within `tolerance`.
///
/// This is how the body margin is found: cluster the left edges of a column's
/// lines and take the biggest cluster. It has to be a *mode* and not a minimum,
/// because the minimum would be whichever line happens to poke furthest left,
/// and it has to be recomputed per page — page sizes in the Turkish test book
/// drift from 474x788 to 545x866 points, so the margin moves with them.
pub fn mode_within(vals: &[f32], tolerance: f32) -> f32 {
    if vals.is_empty() {
        return 0.0;
    }
    let mut v: Vec<f32> = vals.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let mut best_start = 0usize;
    let mut best_len = 0usize;
    let mut start = 0usize;
    for i in 0..v.len() {
        while v[i] - v[start] > tolerance {
            start += 1;
        }
        let len = i - start + 1;
        if len > best_len {
            best_len = len;
            best_start = start;
        }
    }
    let cluster = &v[best_start..best_start + best_len];
    cluster.iter().sum::<f32>() / cluster.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_handles_even_and_odd() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median(&[]), 0.0);
    }

    #[test]
    fn mode_finds_body_margin_not_the_leftmost_line() {
        // Six continuation lines near 31.0 and three indented starts near 54.0,
        // plus one stray line that pokes out to 28.0. The margin is 31, and the
        // stray must not win.
        let lefts = [31.0, 31.2, 30.9, 31.1, 31.0, 30.8, 54.0, 54.2, 53.8, 28.0];
        let m = mode_within(&lefts, 2.0);
        assert!((m - 31.0).abs() < 0.5, "got {m}");
    }

    #[test]
    fn mode_of_page300_style_values() {
        // Page 300 of the Turkish book: margin ~28.5, indent ~47.
        let lefts = [28.56, 28.56, 28.55, 28.81, 28.81, 47.04, 48.49];
        let m = mode_within(&lefts, 2.0);
        assert!((m - 28.6).abs() < 0.5, "got {m}");
    }

    #[test]
    fn rect_union_and_overlap() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(20.0, 0.0, 30.0, 10.0);
        assert_eq!(a.union(&b), Rect::new(0.0, 0.0, 30.0, 10.0));
        assert_eq!(a.x_overlap(&b), -10.0);
    }
}
