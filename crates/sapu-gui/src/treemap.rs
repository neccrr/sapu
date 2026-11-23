//! Squarified treemap layout (Bruls, Huizing & van Wijk, 2000).
//!
//! Items are placed in rows along the shorter side of the remaining space;
//! an item joins the current row only if that keeps the row's worst aspect
//! ratio from getting worse. The result: tiles close to squares, which are
//! much easier to read and click than thin slices.

use eframe::egui::{pos2, vec2, Rect};

/// `values` must be positive and sorted largest-first.
/// Returns one rect per value, in the same order.
pub fn squarify(values: &[f64], rect: Rect) -> Vec<Rect> {
    let mut out = Vec::with_capacity(values.len());
    let total: f64 = values.iter().sum();
    let area = (rect.width() * rect.height()) as f64;
    if values.is_empty() || total <= 0.0 || area <= 0.0 {
        return out;
    }
    let scale = area / total;
    let areas: Vec<f64> = values.iter().map(|v| v * scale).collect();

    let mut free = rect;
    let mut i = 0;
    while i < areas.len() {
        let short = free.width().min(free.height()) as f64;
        if short <= 0.0 {
            // No room left (float rounding): give the rest zero-size rects.
            out.extend(std::iter::repeat_n(Rect::from_min_size(free.min, vec2(0.0, 0.0)), areas.len() - i));
            break;
        }

        // Grow the row while the worst aspect ratio improves.
        let mut end = i + 1;
        let mut best = worst(&areas[i..end], short);
        while end < areas.len() {
            let w = worst(&areas[i..end + 1], short);
            if w > best {
                break;
            }
            best = w;
            end += 1;
        }

        let row = &areas[i..end];
        let row_sum: f64 = row.iter().sum();
        if free.width() >= free.height() {
            // Column on the left side.
            let col_w = (row_sum / free.height() as f64) as f32;
            let mut y = free.top();
            for a in row {
                let h = (*a / col_w as f64) as f32;
                out.push(Rect::from_min_size(pos2(free.left(), y), vec2(col_w, h)));
                y += h;
            }
            free.min.x += col_w;
        } else {
            // Row along the top.
            let row_h = (row_sum / free.width() as f64) as f32;
            let mut x = free.left();
            for a in row {
                let w = (*a / row_h as f64) as f32;
                out.push(Rect::from_min_size(pos2(x, free.top()), vec2(w, row_h)));
                x += w;
            }
            free.min.y += row_h;
        }
        i = end;
    }
    out
}

/// Worst aspect ratio in a row laid along a side of length `side`.
fn worst(row: &[f64], side: f64) -> f64 {
    let sum: f64 = row.iter().sum();
    let (max, min) = row.iter().fold((f64::MIN, f64::MAX), |(hi, lo), &v| (hi.max(v), lo.min(v)));
    let s2 = sum * sum;
    let w2 = side * side;
    (w2 * max / s2).max(s2 / (w2 * min))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_area_exactly() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0));
        let vals = [6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0];
        let rects = squarify(&vals, rect);
        assert_eq!(rects.len(), vals.len());
        let covered: f32 = rects.iter().map(|r| r.area()).sum();
        assert!((covered - rect.area()).abs() < 1.0);
        for r in &rects {
            assert!(rect.expand(0.01).contains_rect(*r));
        }
    }
}
