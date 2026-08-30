use crate::herdr::{self, Rect};

pub fn capture(probe_pane: &str) -> Snapshot {
    herdr::tab_layout(probe_pane)
        .map(|l| Snapshot::capture(&l))
        .unwrap_or_default()
}

#[derive(Default)]
pub struct Snapshot {
    splits: Vec<(String, String, f64, Rect)>,
}

impl Snapshot {
    fn capture(layout: &herdr::TabLayout) -> Self {
        Snapshot {
            splits: layout
                .splits
                .iter()
                .map(|s| (s.id.clone(), s.direction.clone(), s.ratio, s.rect))
                .collect(),
        }
    }
}

pub fn exit_restore(snapshot: &Snapshot, probes: &[&str]) {
    if snapshot.splits.is_empty() {
        return;
    }
    for _ in 0..4 {
        let Some(cur) = probes.iter().find_map(|p| herdr::tab_layout(p).ok()) else {
            return;
        };
        let mut pending = false;
        for (id, _, ratio, _) in &snapshot.splits {
            let Some(c) = cur.splits.iter().find(|s| &s.id == id) else {
                continue;
            };
            let axis = if c.direction == "right" { c.rect.width } else { c.rect.height };
            if axis <= 0 {
                continue;
            }
            let eps = 1.0 / axis as f64;
            let delta = *ratio - c.ratio;
            if delta.abs() <= eps {
                continue;
            }
            if let Some(pane) = neighbor(&cur, c, delta > 0.0) {
                let dir = if delta > 0.0 {
                    c.direction.clone()
                } else {
                    match c.direction.as_str() {
                        "right" => "left".to_string(),
                        _ => "up".to_string(),
                    }
                };
                if herdr::resize_pane(&pane, &dir, delta.abs()).unwrap_or(false) {
                    pending = true;
                }
            }
        }
        if !pending {
            return;
        }
    }
}

fn neighbor(layout: &herdr::TabLayout, split: &herdr::Split, low_side: bool) -> Option<String> {
    let (bx, by) = match split.direction.as_str() {
        "right" => (
            split.rect.x + (split.rect.width as f64 * split.ratio).round() as i32,
            split.rect.y,
        ),
        _ => (
            split.rect.x,
            split.rect.y + (split.rect.height as f64 * split.ratio).round() as i32,
        ),
    };
    layout.panes.iter().find_map(|(id, r)| {
        let inside = r.x >= split.rect.x
            && r.y >= split.rect.y
            && r.x + r.width <= split.rect.x + split.rect.width
            && r.y + r.height <= split.rect.y + split.rect.height;
        let edge = if split.direction == "right" {
            if low_side {
                r.x + r.width == bx
            } else {
                r.x == bx
            }
        } else if low_side {
            r.y + r.height == by
        } else {
            r.y == by
        };
        (inside && edge).then(|| id.clone())
    })
}
