use std::collections::HashMap;

use anyhow::Result;

use crate::herdr::{self, Rect};

const MAX_STEPS_PER_PANE: usize = 10;
const TOLERANCE_CELLS: i32 = 2;
const MIN_STEP: f64 = 0.02;
const MAX_STEP: f64 = 0.4;

#[derive(Clone, Copy)]
struct Axis {
    size: i32,
    area: i32,
}

fn step_axis(
    pane_id: &str,
    axis: &'static str,
    low_dir: &'static str,
    high_dir: &'static str,
    goal: i32,
    cur: Axis,
    signs: &mut HashMap<(String, &'static str), &'static str>,
) -> Result<bool> {
    let delta = goal - cur.size;
    if delta.abs() <= TOLERANCE_CELLS || cur.area <= 0 {
        return Ok(false);
    }
    let ratio_delta = (delta as f64 / cur.area as f64)
        .clamp(-MAX_STEP, MAX_STEP)
        .max(if delta > 0 { MIN_STEP } else { -MIN_STEP });
    let key = (pane_id.to_string(), axis);
    let dir = *signs.entry(key.clone()).or_insert(high_dir);
    let mut moved = herdr::resize_pane(pane_id, dir, ratio_delta.abs())?;
    let layout = herdr::tab_layout(pane_id)?;
    let new_size = layout
        .panes
        .iter()
        .find(|(id, _)| id == pane_id)
        .map(|(_, r)| if axis == "x" { r.width } else { r.height })
        .unwrap_or(cur.size);
    let improved = (goal - new_size).abs() < delta.abs();
    if !improved {
        let flipped = if dir == low_dir { high_dir } else { low_dir };
        signs.insert(key, flipped);
        if moved {
            herdr::resize_pane(pane_id, flipped, ratio_delta.abs())?;
            moved = true;
        }
    }
    Ok(moved)
}

fn drive(pane_id: &str, goal: Rect, signs: &mut HashMap<(String, &'static str), &'static str>) -> Result<()> {
    for _ in 0..MAX_STEPS_PER_PANE {
        let layout = herdr::tab_layout(pane_id)?;
        let Some((_, rect)) = layout.panes.iter().find(|(id, _)| id == pane_id) else {
            return Ok(());
        };
        let cur = *rect;
        let moved_x = step_axis(
            pane_id,
            "x",
            "left",
            "right",
            goal.width,
            Axis { size: cur.width, area: layout.area.width },
            signs,
        )?;
        let moved_y = step_axis(
            pane_id,
            "y",
            "up",
            "down",
            goal.height,
            Axis { size: cur.height, area: layout.area.height },
            signs,
        )?;
        if !moved_x && !moved_y {
            return Ok(());
        }
    }
    Ok(())
}

pub fn drive_all(goals: &[(String, Rect)]) -> Result<()> {
    let mut signs = HashMap::new();
    for (pane_id, goal) in goals {
        drive(pane_id, *goal, &mut signs)?;
    }
    Ok(())
}

pub fn rect_of(layout: &herdr::TabLayout, pane_id: &str) -> Option<Rect> {
    layout.panes.iter().find(|(id, _)| id == pane_id).map(|(_, r)| *r)
}

pub fn aligned(layout: &herdr::TabLayout, pane_id: &str, goal: Rect) -> bool {
    match rect_of(layout, pane_id) {
        Some(r) => {
            (r.width - goal.width).abs() <= TOLERANCE_CELLS
                && (r.height - goal.height).abs() <= TOLERANCE_CELLS
        }
        None => false,
    }
}
