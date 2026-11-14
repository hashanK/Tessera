//! Grid geometry for a group. Everything is in physical pixels of the group window.
use crate::look::LabelPos;
use crate::model::{Rect, Style};

/// Design-token sizes scaled to a DPI. Group geometry (the stored rectangle) never includes
/// `margin`: that is extra room around the window for the shadow.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub scale: f32,
    pub icon: i32,
    pub pad: i32,
    pub title_h: i32,
    pub cell_w: i32,
    pub cell_h: i32,
    pub radius: i32,
    pub grip: i32,
    /// Height of the label box under (or beside) an icon.
    pub label_h: i32,
    /// Labels sit to the right of the icon (a list) rather than below it.
    pub label_right: bool,
    /// Shadow room on every side of the group window.
    pub margin: i32,
}

impl Metrics {
    pub fn new(style: &Style, dpi: u32) -> Self {
        let scale = dpi.max(96) as f32 / 96.0;
        let s = |v: f32| (v * scale).round() as i32;
        let icon = s(style.icon as f32);
        let lines = style.text.lines.clamp(1, 3) as f32;
        let label_h = s(style.text.line_px() * lines);
        let spacing = style.spacing as f32;
        let label_right = style.icons.label == LabelPos::Right;
        let (cell_w, cell_h) = if label_right {
            // Icon, gap, and a label column wide enough for a typical name.
            (icon + s(spacing * 0.5 + 10.0 + (style.text.size * 11.0).max(110.0)), icon.max(label_h) + s(14.0 + (spacing - 28.0).max(-12.0) / 3.0))
        } else {
            // 8 above the icon, 4 between icon and label, ~6 below: the original 50 for 12 px text.
            (icon + s(spacing), icon + label_h + s(18.0 + (spacing - 28.0) / 2.0))
        };
        Metrics {
            scale,
            icon,
            pad: s(style.padding as f32),
            title_h: s(style.title.height as f32),
            cell_w,
            cell_h,
            radius: s(style.radius as f32),
            grip: s(6.0),
            label_h,
            label_right,
            margin: s(style.shadow.margin() as f32),
        }
    }

    pub fn px(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    pub fn min_size(&self) -> (i32, i32) {
        (self.cell_w + 2 * self.pad, self.title_h + self.cell_h + self.pad)
    }

    /// Window size that shows exactly `cols` x `rows` cells.
    pub fn size_for(&self, cols: i32, rows: i32) -> (i32, i32) {
        (cols.max(1) * self.cell_w + 2 * self.pad, self.title_h + rows.max(1) * self.cell_h + self.pad)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub cols: i32,
    pub rows: i32,
    /// Width of one column slot; at least `cell_w`, wider when the group is justified.
    pub slot_w: i32,
    pub content_h: i32,
}

pub fn grid(m: &Metrics, width: i32, count: usize) -> Grid {
    let inner = (width - 2 * m.pad).max(m.cell_w);
    let cols = (inner / m.cell_w).max(1);
    let rows = ((count as i32) + cols - 1) / cols;
    Grid { cols, rows, slot_w: inner / cols, content_h: rows * m.cell_h }
}

pub fn max_scroll(m: &Metrics, g: &Grid, height: i32) -> i32 {
    (g.content_h - (height - m.title_h - m.pad)).max(0)
}

/// Rectangle of cell `index` in window coordinates.
pub fn cell_rect(m: &Metrics, g: &Grid, index: usize, scroll: i32) -> Rect {
    let (col, row) = (index as i32 % g.cols, index as i32 / g.cols);
    Rect { x: m.pad + col * g.slot_w + (g.slot_w - m.cell_w) / 2, y: m.title_h + row * m.cell_h - scroll, w: m.cell_w, h: m.cell_h }
}

/// Item under a point, if any.
pub fn hit(m: &Metrics, g: &Grid, x: i32, y: i32, scroll: i32, count: usize) -> Option<usize> {
    if y < m.title_h || x < m.pad {
        return None;
    }
    let col = (x - m.pad) / g.slot_w;
    let row = (y - m.title_h + scroll) / m.cell_h;
    if col >= g.cols {
        return None;
    }
    let index = (row * g.cols + col) as usize;
    (index < count).then_some(index)
}

/// Position at which a dropped item would be inserted (0..=count).
pub fn insert_index(m: &Metrics, g: &Grid, x: i32, y: i32, scroll: i32, count: usize) -> usize {
    let row = ((y - m.title_h + scroll).max(0)) / m.cell_h;
    let col = (((x - m.pad).max(0) + g.slot_w / 2) / g.slot_w).min(g.cols);
    ((row * g.cols + col).max(0) as usize).min(count)
}

/// Shelf-pack rectangles of the given sizes into `area`, left to right, top to bottom.
pub fn shelf_pack(sizes: &[(i32, i32)], area: Rect, margin: i32, gap: i32) -> Vec<Rect> {
    let (mut x, mut y, mut shelf_h) = (area.x + margin, area.y + margin, 0);
    sizes
        .iter()
        .map(|&(w, h)| {
            if x > area.x + margin && x + w > area.x + area.w - margin {
                x = area.x + margin;
                y += shelf_h + gap;
                shelf_h = 0;
            }
            let r = Rect { x, y, w, h };
            x += w + gap;
            shelf_h = shelf_h.max(h);
            r
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_wraps_and_hits() {
        let m = Metrics::new(&Style::default(), 96);
        assert_eq!((m.cell_w, m.cell_h), (76, 98));
        let (w, h) = m.size_for(3, 2);
        let g = grid(&m, w, 5);
        assert_eq!((g.cols, g.rows), (3, 2));
        assert_eq!(max_scroll(&m, &g, h), 0);
        let r = cell_rect(&m, &g, 4, 0);
        assert_eq!(hit(&m, &g, r.x + 5, r.y + 5, 0, 5), Some(4));
        assert_eq!(hit(&m, &g, r.x + m.cell_w + 5, r.y + 5, 0, 5), None, "empty sixth slot");
        assert_eq!(hit(&m, &g, 5, 5, 0, 5), None, "title bar");
    }

    #[test]
    fn scroll_limit_and_insert() {
        let m = Metrics::new(&Style::default(), 96);
        let (w, h) = m.size_for(2, 1);
        let g = grid(&m, w, 6);
        assert_eq!(max_scroll(&m, &g, h), 2 * m.cell_h);
        assert_eq!(insert_index(&m, &g, 0, m.title_h + 1, 0, 6), 0);
        assert_eq!(insert_index(&m, &g, w, m.title_h + 1, 0, 6), 2);
        assert_eq!(insert_index(&m, &g, w, 10_000, 0, 6), 6);
    }

    #[test]
    fn dpi_scales_metrics() {
        let m = Metrics::new(&Style::default(), 144);
        assert_eq!(m.icon, 72);
        assert_eq!(m.title_h, 45);
    }

    #[test]
    fn shelf_pack_wraps() {
        let area = Rect { x: 0, y: 0, w: 500, h: 500 };
        let r = shelf_pack(&[(200, 100), (200, 150), (200, 100)], area, 10, 10);
        assert_eq!((r[1].x, r[1].y), (220, 10));
        assert_eq!((r[2].x, r[2].y), (10, 170));
    }
}
