use anyhow::{Result, bail};
use image::{DynamicImage, GrayImage, ImageBuffer, Luma, Rgb, RgbImage};
use serde::{Deserialize, Serialize};
use std::cmp::{Reverse, max, min};
use std::collections::VecDeque;
use std::path::Path;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PanMode {
    Auto,
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Shift {
    /// Content displacement on screen from previous frame to this frame.
    /// Positive dx means content appears farther right in the later frame.
    /// Positive dy means content appears lower in the later frame.
    pub dx: i32,
    pub dy: i32,
    pub score: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Position {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateBand {
    pub score: f32,
    pub y1: u32,
    pub y2: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicatePatch {
    pub score: f32,
    pub rect_a: [u32; 4],
    pub rect_b: [u32; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateReport {
    pub passed: bool,
    pub band_duplicates: Vec<DuplicateBand>,
    pub patch_duplicates: Vec<DuplicatePatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StitchReport {
    pub shifts: Vec<Shift>,
    pub raw_positions: Vec<Position>,
    pub normalized_positions: Vec<Position>,
    pub canvas_width: u32,
    pub canvas_height: u32,
    pub duplicate_report: Option<DuplicateReport>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SourceSelection {
    NearestCenter,
    SeamDp,
}

#[derive(Debug, Clone)]
pub struct StitchOptions {
    pub mode: PanMode,
    pub source_selection: SourceSelection,
    pub min_shift_y: i32,
    pub max_shift_y: Option<i32>,
    pub min_shift_x: i32,
    pub max_shift_x: Option<i32>,
    pub max_drift_x: i32,
    pub max_drift_y: i32,
    pub align_scale: usize,
    pub snap_x: i32,
    pub check_duplicates: bool,
}

impl Default for StitchOptions {
    fn default() -> Self {
        Self {
            mode: PanMode::Auto,
            source_selection: SourceSelection::NearestCenter,
            min_shift_y: 30,
            max_shift_y: None,
            min_shift_x: 30,
            max_shift_x: None,
            max_drift_x: 60,
            max_drift_y: 60,
            align_scale: 4,
            snap_x: 3,
            check_duplicates: false,
        }
    }
}

pub fn load_images(paths: &[impl AsRef<Path>]) -> Result<Vec<RgbImage>> {
    paths
        .iter()
        .map(|p| {
            image::open(p.as_ref())
                .map(DynamicImage::into_rgb8)
                .map_err(Into::into)
        })
        .collect()
}

pub fn stitch_images(
    images: &[RgbImage],
    opts: &StitchOptions,
) -> Result<(RgbImage, StitchReport)> {
    let (stitched, _source_map, report) = stitch_images_with_source_map(images, opts)?;
    Ok((stitched, report))
}

pub fn stitch_images_with_source_map(
    images: &[RgbImage],
    opts: &StitchOptions,
) -> Result<(RgbImage, SourceMap, StitchReport)> {
    if images.len() < 2 {
        bail!("need at least two input frames");
    }

    let (w, h) = images[0].dimensions();
    for (idx, img) in images.iter().enumerate() {
        if img.dimensions() != (w, h) {
            bail!(
                "all images must have identical dimensions; frame 0 is {}x{}, frame {} is {}x{}",
                w,
                h,
                idx,
                img.width(),
                img.height()
            );
        }
    }

    let max_shift_y = opts.max_shift_y.unwrap_or((h as f32 * 0.75).round() as i32);
    let max_shift_x = opts.max_shift_x.unwrap_or((w as f32 * 0.75).round() as i32);
    let shifts = estimate_shifts(
        images,
        opts.align_scale,
        opts.mode,
        opts.min_shift_y,
        max_shift_y,
        opts.min_shift_x,
        max_shift_x,
        opts.max_drift_x,
        opts.max_drift_y,
        opts.snap_x,
    )?;

    let raw_positions = positions_from_shifts(&shifts);
    let (positions, canvas_w, canvas_h) = normalize_positions(&raw_positions, w, h);
    let (stitched, source_map) = match opts.source_selection {
        SourceSelection::NearestCenter => {
            synthesize_nearest_center_with_source_map(images, &positions, canvas_w, canvas_h)?
        }
        SourceSelection::SeamDp => {
            synthesize_vertical_seams_with_source_map(images, &positions, canvas_w, canvas_h)?
        }
    };
    let duplicate_report = opts.check_duplicates.then(|| detect_duplicates(&stitched));
    if let Some(report) = &duplicate_report
        && !report.passed
    {
        bail!("DUPLICATE TEST: FAIL");
    }

    let report = StitchReport {
        shifts,
        raw_positions,
        normalized_positions: positions,
        canvas_width: canvas_w,
        canvas_height: canvas_h,
        duplicate_report,
    };
    Ok((stitched, source_map, report))
}

#[allow(clippy::too_many_arguments)]
pub fn estimate_shifts(
    images: &[RgbImage],
    scale: usize,
    mode: PanMode,
    min_y: i32,
    max_y: i32,
    min_x: i32,
    max_x: i32,
    max_drift_x: i32,
    max_drift_y: i32,
    snap_x: i32,
) -> Result<Vec<Shift>> {
    let prepped: Vec<GrayF32> = images
        .iter()
        .map(|img| prep_alignment_image(img, scale))
        .collect();
    let mut shifts = Vec::new();
    for i in 0..prepped.len() - 1 {
        let (mut best_dx, best_dy, score) = find_pair_shift(
            &prepped[i],
            &prepped[i + 1],
            scale,
            mode,
            min_y,
            max_y,
            min_x,
            max_x,
            max_drift_x,
            max_drift_y,
        )?;
        if best_dx.abs() <= snap_x {
            best_dx = 0;
        }
        shifts.push(Shift {
            dx: best_dx,
            dy: best_dy,
            score,
        });
    }
    Ok(shifts)
}

#[derive(Clone)]
struct GrayF32 {
    w: usize,
    h: usize,
    data: Vec<f32>,
}

impl GrayF32 {
    fn get(&self, x: usize, y: usize) -> f32 {
        self.data[y * self.w + x]
    }
}

fn prep_alignment_image(img: &RgbImage, scale: usize) -> GrayF32 {
    let w0 = img.width() as usize;
    let h0 = img.height() as usize;
    let w = max(1, w0 / scale);
    let h = max(1, h0 / scale);
    let mut gray = vec![0.0f32; w * h];

    for y in 0..h {
        for x in 0..w {
            let mut sum = 0.0;
            let mut n = 0.0;
            for yy in y * scale..min((y + 1) * scale, h0) {
                for xx in x * scale..min((x + 1) * scale, w0) {
                    let p = img.get_pixel(xx as u32, yy as u32).0;
                    sum += 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32;
                    n += 1.0;
                }
            }
            gray[y * w + x] = sum / n;
        }
    }

    let mut edge = vec![0.0f32; w * h];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let gx = -gray[(y - 1) * w + x - 1] + gray[(y - 1) * w + x + 1]
                - 2.0 * gray[y * w + x - 1]
                + 2.0 * gray[y * w + x + 1]
                - gray[(y + 1) * w + x - 1]
                + gray[(y + 1) * w + x + 1];
            let gy = -gray[(y - 1) * w + x - 1]
                - 2.0 * gray[(y - 1) * w + x]
                - gray[(y - 1) * w + x + 1]
                + gray[(y + 1) * w + x - 1]
                + 2.0 * gray[(y + 1) * w + x]
                + gray[(y + 1) * w + x + 1];
            edge[y * w + x] = (gx * gx + gy * gy).sqrt();
        }
    }
    standardize(&mut edge);
    GrayF32 { w, h, data: edge }
}

fn standardize(v: &mut [f32]) {
    if v.is_empty() {
        return;
    }
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    let var = v.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / v.len() as f32;
    let std = var.sqrt().max(1e-6);
    for x in v {
        *x = (*x - mean) / std;
    }
}

#[allow(clippy::too_many_arguments)]
fn find_pair_shift(
    a: &GrayF32,
    b: &GrayF32,
    scale: usize,
    mode: PanMode,
    min_y: i32,
    max_y: i32,
    min_x: i32,
    max_x: i32,
    max_drift_x: i32,
    max_drift_y: i32,
) -> Result<(i32, i32, f32)> {
    if a.w != b.w || a.h != b.h {
        bail!("alignment images have mismatched dimensions");
    }

    let min_sy = max(1, min_y / scale as i32);
    let max_sy = max(min_sy + 1, max_y / scale as i32);
    let min_sx = max(1, min_x / scale as i32);
    let max_sx = max(min_sx + 1, max_x / scale as i32);
    let drift_sx = max(0, max_drift_x / scale as i32);
    let drift_sy = max(0, max_drift_y / scale as i32);

    let mut best = (0, 0, f32::NEG_INFINITY);
    for dy in -(a.h as i32 - 20)..=(a.h as i32 - 20) {
        for dx in -(a.w as i32 - 20)..=(a.w as i32 - 20) {
            if !candidate_allowed_scaled(
                mode, dx, dy, min_sx, max_sx, min_sy, max_sy, drift_sx, drift_sy,
            ) {
                continue;
            }
            let score = ncc_content_shift(a, b, dx, dy);
            if score > best.2 {
                best = (dx, dy, score);
            }
        }
    }

    if !best.2.is_finite() {
        bail!("could not find a valid pan shift for mode {:?}", mode);
    }

    let mut best_full = (best.0 * scale as i32, best.1 * scale as i32, best.2);
    for dy_full in (best_full.1 - scale as i32 * 3)..=(best_full.1 + scale as i32 * 3) {
        for dx_full in (best_full.0 - scale as i32 * 3)..=(best_full.0 + scale as i32 * 3) {
            if !candidate_allowed_full(
                mode,
                dx_full,
                dy_full,
                min_x,
                max_x,
                min_y,
                max_y,
                max_drift_x,
                max_drift_y,
            ) {
                continue;
            }
            let dx = (dx_full as f32 / scale as f32).round() as i32;
            let dy = (dy_full as f32 / scale as f32).round() as i32;
            let score = ncc_content_shift(a, b, dx, dy);
            if score > best_full.2 {
                best_full = (dx_full, dy_full, score);
            }
        }
    }

    Ok(best_full)
}

#[allow(clippy::too_many_arguments)]
fn candidate_allowed_scaled(
    mode: PanMode,
    dx: i32,
    dy: i32,
    min_sx: i32,
    max_sx: i32,
    min_sy: i32,
    max_sy: i32,
    drift_sx: i32,
    drift_sy: i32,
) -> bool {
    let vertical = dy.abs() >= min_sy && dy.abs() <= max_sy && dx.abs() <= drift_sx;
    let horizontal = dx.abs() >= min_sx && dx.abs() <= max_sx && dy.abs() <= drift_sy;
    match mode {
        PanMode::Auto => vertical || horizontal,
        PanMode::Vertical => vertical,
        PanMode::Horizontal => horizontal,
    }
}

#[allow(clippy::too_many_arguments)]
fn candidate_allowed_full(
    mode: PanMode,
    dx: i32,
    dy: i32,
    min_x: i32,
    max_x: i32,
    min_y: i32,
    max_y: i32,
    max_drift_x: i32,
    max_drift_y: i32,
) -> bool {
    let vertical = dy.abs() >= min_y && dy.abs() <= max_y && dx.abs() <= max_drift_x;
    let horizontal = dx.abs() >= min_x && dx.abs() <= max_x && dy.abs() <= max_drift_y;
    match mode {
        PanMode::Auto => vertical || horizontal,
        PanMode::Vertical => vertical,
        PanMode::Horizontal => horizontal,
    }
}

fn ncc_content_shift(a: &GrayF32, b: &GrayF32, dx: i32, dy: i32) -> f32 {
    let w = a.w as i32;
    let h = a.h as i32;
    if dx.abs() >= w - 10 || dy.abs() >= h - 10 || (dx == 0 && dy == 0) {
        return f32::NEG_INFINITY;
    }

    let ax0 = max(0, -dx);
    let ay0 = max(0, -dy);
    let ax1 = min(w, w - dx);
    let ay1 = min(h, h - dy);
    let ow = ax1 - ax0;
    let oh = ay1 - ay0;
    if ow < 40 || oh < 40 {
        return f32::NEG_INFINITY;
    }
    let bx0 = ax0 + dx;
    let by0 = ay0 + dy;

    let mut n = 0.0f32;
    let mut sum_a = 0.0;
    let mut sum_b = 0.0;
    for y in 0..oh {
        for x in 0..ow {
            sum_a += a.get((ax0 + x) as usize, (ay0 + y) as usize);
            sum_b += b.get((bx0 + x) as usize, (by0 + y) as usize);
            n += 1.0;
        }
    }
    let mean_a = sum_a / n;
    let mean_b = sum_b / n;

    let mut num = 0.0;
    let mut den_a = 0.0;
    let mut den_b = 0.0;
    for y in 0..oh {
        for x in 0..ow {
            let va = a.get((ax0 + x) as usize, (ay0 + y) as usize) - mean_a;
            let vb = b.get((bx0 + x) as usize, (by0 + y) as usize) - mean_b;
            num += va * vb;
            den_a += va * va;
            den_b += vb * vb;
        }
    }
    num / (den_a.sqrt() * den_b.sqrt()).max(1e-6)
}

pub fn positions_from_shifts(shifts: &[Shift]) -> Vec<Position> {
    let mut pos = vec![Position { x: 0, y: 0 }];
    let mut x = 0;
    let mut y = 0;
    for s in shifts {
        x -= s.dx;
        y -= s.dy;
        pos.push(Position { x, y });
    }
    pos
}

pub fn normalize_positions(raw: &[Position], w: u32, h: u32) -> (Vec<Position>, u32, u32) {
    let min_x = raw.iter().map(|p| p.x).min().unwrap_or(0);
    let min_y = raw.iter().map(|p| p.y).min().unwrap_or(0);
    let normalized: Vec<_> = raw
        .iter()
        .map(|p| Position {
            x: p.x - min_x,
            y: p.y - min_y,
        })
        .collect();
    let max_x = normalized
        .iter()
        .map(|p| p.x + w as i32)
        .max()
        .unwrap_or(w as i32);
    let max_y = normalized
        .iter()
        .map(|p| p.y + h as i32)
        .max()
        .unwrap_or(h as i32);
    (normalized, max_x as u32, max_y as u32)
}

pub fn synthesize_nearest_center(
    images: &[RgbImage],
    positions: &[Position],
    canvas_w: u32,
    canvas_h: u32,
) -> Result<RgbImage> {
    Ok(synthesize_nearest_center_with_source_map(images, positions, canvas_w, canvas_h)?.0)
}

pub fn synthesize_nearest_center_with_source_map(
    images: &[RgbImage],
    positions: &[Position],
    canvas_w: u32,
    canvas_h: u32,
) -> Result<(RgbImage, SourceMap)> {
    let (w, h) = images[0].dimensions();
    let center_x = w as f32 / 2.0;
    let center_y = h as f32 / 2.0;
    let mut out = ImageBuffer::from_pixel(canvas_w, canvas_h, Rgb([0, 0, 0]));
    let mut source_map = SourceMap::new(canvas_w, canvas_h);
    for cy in 0..canvas_h as i32 {
        for cx in 0..canvas_w as i32 {
            let mut best: Option<(f32, usize, u32, u32)> = None;
            for (i, p) in positions.iter().enumerate() {
                let sx = cx - p.x;
                let sy = cy - p.y;
                if sx >= 0 && sy >= 0 && sx < w as i32 && sy < h as i32 {
                    let score = ((sx as f32 - center_x).powi(2) + (sy as f32 - center_y).powi(2))
                        .sqrt()
                        - i as f32 * 0.01;
                    if best.map(|b| score < b.0).unwrap_or(true) {
                        best = Some((score, i, sx as u32, sy as u32));
                    }
                }
            }
            if let Some((_score, i, sx, sy)) = best {
                out.put_pixel(cx as u32, cy as u32, *images[i].get_pixel(sx, sy));
                source_map.set(cx as u32, cy as u32, i as u8);
            }
        }
    }
    Ok((out, source_map))
}

pub fn synthesize_vertical_seams_with_source_map(
    images: &[RgbImage],
    positions: &[Position],
    canvas_w: u32,
    canvas_h: u32,
) -> Result<(RgbImage, SourceMap)> {
    if images.is_empty() {
        bail!("need at least one image");
    }
    let (w, h) = images[0].dimensions();
    // This first version is for mostly vertical pans. If the stack is not mostly vertical,
    // fall back to the proven nearest-center method.
    let span_x = positions.iter().map(|p| p.x).max().unwrap_or(0)
        - positions.iter().map(|p| p.x).min().unwrap_or(0);
    let span_y = positions.iter().map(|p| p.y).max().unwrap_or(0)
        - positions.iter().map(|p| p.y).min().unwrap_or(0);
    if span_y < span_x {
        return synthesize_nearest_center_with_source_map(images, positions, canvas_w, canvas_h);
    }

    let mut order: Vec<usize> = (0..images.len()).collect();
    order.sort_by_key(|&i| positions[i].y);
    let first = order[0];
    let mut canvas = ImageBuffer::from_pixel(canvas_w, canvas_h, Rgb([0, 0, 0]));
    let mut source_map = SourceMap::new(canvas_w, canvas_h);
    paste_full(
        &mut canvas,
        &mut source_map,
        &images[first],
        positions[first],
        first as u8,
    );

    for &idx in order.iter().skip(1) {
        let pos = positions[idx];
        let overlap = overlap_rect_with_existing(&source_map, pos, w, h);
        if let Some((x0, y0, x1, y1)) = overlap {
            let seam = find_vertical_pan_seam(&canvas, &images[idx], pos, x0, y0, x1, y1);
            paste_with_seam(
                &mut canvas,
                &mut source_map,
                &images[idx],
                pos,
                idx as u8,
                &seam,
                x0,
                y0,
                x1,
                y1,
            );
        } else {
            paste_full(&mut canvas, &mut source_map, &images[idx], pos, idx as u8);
        }
    }

    Ok((canvas, source_map))
}

fn paste_full(
    canvas: &mut RgbImage,
    source_map: &mut SourceMap,
    img: &RgbImage,
    pos: Position,
    source: u8,
) {
    for sy in 0..img.height() {
        for sx in 0..img.width() {
            let cx = pos.x + sx as i32;
            let cy = pos.y + sy as i32;
            if cx >= 0 && cy >= 0 && cx < canvas.width() as i32 && cy < canvas.height() as i32 {
                canvas.put_pixel(cx as u32, cy as u32, *img.get_pixel(sx, sy));
                source_map.set(cx as u32, cy as u32, source);
            }
        }
    }
}

fn overlap_rect_with_existing(
    source_map: &SourceMap,
    pos: Position,
    w: u32,
    h: u32,
) -> Option<(u32, u32, u32, u32)> {
    let x0 = max(0, pos.x) as u32;
    let y0 = max(0, pos.y) as u32;
    let x1 = min(source_map.width as i32, pos.x + w as i32) as u32;
    let y1 = min(source_map.height as i32, pos.y + h as i32) as u32;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let mut min_x = x1;
    let mut min_y = y1;
    let mut max_x = x0;
    let mut max_y = y0;
    let mut found = false;
    for y in y0..y1 {
        for x in x0..x1 {
            if source_map.get(x, y) != SourceMap::UNASSIGNED {
                found = true;
                min_x = min(min_x, x);
                min_y = min(min_y, y);
                max_x = max(max_x, x);
                max_y = max(max_y, y);
            }
        }
    }
    found.then_some((min_x, min_y, max_x + 1, max_y + 1))
}

fn find_vertical_pan_seam(
    canvas: &RgbImage,
    incoming: &RgbImage,
    pos: Position,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
) -> Vec<u32> {
    let ow = (x1 - x0) as usize;
    let oh = (y1 - y0) as usize;
    if ow == 0 || oh == 0 {
        return Vec::new();
    }
    let mut cost = vec![0.0f32; ow * oh];
    for yy in 0..oh {
        for xx in 0..ow {
            let cx = x0 + xx as u32;
            let cy = y0 + yy as u32;
            let sx = (cx as i32 - pos.x) as u32;
            let sy = (cy as i32 - pos.y) as u32;
            let a = canvas.get_pixel(cx, cy).0;
            let b = incoming.get_pixel(sx, sy).0;
            let color = rgb_abs_diff(a, b);
            // Prefer seams through places where the two sources agree and avoid strong visible edges.
            let edge = local_luma_edge(canvas, cx, cy) + local_luma_edge_in(incoming, sx, sy);
            let margin = yy.min(oh - 1 - yy) as f32;
            let edge_margin_penalty = if margin < 12.0 {
                (12.0 - margin) * 8.0
            } else {
                0.0
            };
            cost[yy * ow + xx] = color + 0.15 * edge + edge_margin_penalty;
        }
    }

    let mut dp = vec![0.0f32; ow * oh];
    let mut back = vec![0i8; ow * oh];
    for y in 0..oh {
        dp[y * ow] = cost[y * ow];
    }
    for x in 1..ow {
        for y in 0..oh {
            let mut best = dp[y * ow + x - 1];
            let mut best_d = 0i8;
            if y > 0 {
                let v = dp[(y - 1) * ow + x - 1] + 1.5;
                if v < best {
                    best = v;
                    best_d = -1;
                }
            }
            if y + 1 < oh {
                let v = dp[(y + 1) * ow + x - 1] + 1.5;
                if v < best {
                    best = v;
                    best_d = 1;
                }
            }
            dp[y * ow + x] = cost[y * ow + x] + best;
            back[y * ow + x] = best_d;
        }
    }

    let mut seam = vec![0u32; ow];
    let mut y = (0..oh)
        .min_by(|&a, &b| dp[a * ow + ow - 1].total_cmp(&dp[b * ow + ow - 1]))
        .unwrap_or(oh / 2);
    seam[ow - 1] = y0 + y as u32;
    for x in (1..ow).rev() {
        let d = back[y * ow + x];
        y = match d {
            -1 => y + 1,
            1 => y.saturating_sub(1),
            _ => y,
        };
        seam[x - 1] = y0 + y as u32;
    }
    seam
}

fn paste_with_seam(
    canvas: &mut RgbImage,
    source_map: &mut SourceMap,
    incoming: &RgbImage,
    pos: Position,
    source: u8,
    seam: &[u32],
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
) {
    for sy in 0..incoming.height() {
        for sx in 0..incoming.width() {
            let cx = pos.x + sx as i32;
            let cy = pos.y + sy as i32;
            if cx < 0 || cy < 0 || cx >= canvas.width() as i32 || cy >= canvas.height() as i32 {
                continue;
            }
            let cxu = cx as u32;
            let cyu = cy as u32;
            let replace = if source_map.get(cxu, cyu) == SourceMap::UNASSIGNED {
                true
            } else if cxu >= x0 && cxu < x1 && cyu >= y0 && cyu < y1 {
                let seam_y = seam[(cxu - x0) as usize];
                cyu >= seam_y
            } else {
                false
            };
            if replace {
                canvas.put_pixel(cxu, cyu, *incoming.get_pixel(sx, sy));
                source_map.set(cxu, cyu, source);
            }
        }
    }
}

fn rgb_abs_diff(a: [u8; 3], b: [u8; 3]) -> f32 {
    ((a[0] as f32 - b[0] as f32).abs()
        + (a[1] as f32 - b[1] as f32).abs()
        + (a[2] as f32 - b[2] as f32).abs())
        / 3.0
}

fn luma(p: Rgb<u8>) -> f32 {
    0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32
}

fn local_luma_edge(img: &RgbImage, x: u32, y: u32) -> f32 {
    let c = luma(*img.get_pixel(x, y));
    let mut e: f32 = 0.0;
    if x > 0 {
        e = e.max((c - luma(*img.get_pixel(x - 1, y))).abs());
    }
    if y > 0 {
        e = e.max((c - luma(*img.get_pixel(x, y - 1))).abs());
    }
    if x + 1 < img.width() {
        e = e.max((c - luma(*img.get_pixel(x + 1, y))).abs());
    }
    if y + 1 < img.height() {
        e = e.max((c - luma(*img.get_pixel(x, y + 1))).abs());
    }
    e
}

fn local_luma_edge_in(img: &RgbImage, x: u32, y: u32) -> f32 {
    local_luma_edge(img, x, y)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceMap {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl SourceMap {
    pub const UNASSIGNED: u8 = u8::MAX;

    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![Self::UNASSIGNED; (width * height) as usize],
        }
    }

    pub fn get(&self, x: u32, y: u32) -> u8 {
        self.data[(y * self.width + x) as usize]
    }

    pub fn set(&mut self, x: u32, y: u32, value: u8) {
        self.data[(y * self.width + x) as usize] = value;
    }

    pub fn to_gray_image(&self) -> GrayImage {
        let mut img = GrayImage::new(self.width, self.height);
        for y in 0..self.height {
            for x in 0..self.width {
                let v = self.get(x, y);
                img.put_pixel(x, y, Luma([if v == Self::UNASSIGNED { 0 } else { v + 1 }]));
            }
        }
        img
    }

    pub fn from_gray_image(img: &GrayImage) -> Self {
        let (width, height) = img.dimensions();
        let mut map = Self::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let p = img.get_pixel(x, y).0[0];
                map.set(x, y, if p == 0 { Self::UNASSIGNED } else { p - 1 });
            }
        }
        map
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EvalThresholds {
    pub max_high_risk_boundary_pixels: u32,
    pub max_largest_risky_component_area: u32,
    pub max_duplicate_bands: usize,
    pub max_duplicate_patches: usize,
}

impl Default for EvalThresholds {
    fn default() -> Self {
        Self {
            max_high_risk_boundary_pixels: 250,
            max_largest_risky_component_area: 300,
            max_duplicate_bands: 0,
            max_duplicate_patches: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskyComponent {
    pub area_px: u32,
    pub bbox_xywh: [u32; 4],
    pub mean_gradient: f32,
    pub max_gradient: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffPairReport {
    pub sources: [u8; 2],
    pub boundary_px: u32,
    pub risk_px: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationReport {
    pub passed: bool,
    pub image_size: [u32; 2],
    pub boundary_pixels: u32,
    pub high_risk_boundary_pixels: u32,
    pub largest_risky_component_area: u32,
    pub high_risk_components: Vec<RiskyComponent>,
    pub handoff_pairs: Vec<HandoffPairReport>,
    pub duplicate_report: DuplicateReport,
    pub thresholds: EvalThresholds,
    pub failures: Vec<String>,
}

pub fn generate_source_map(
    positions: &[Position],
    input_width: u32,
    input_height: u32,
    canvas_width: u32,
    canvas_height: u32,
) -> SourceMap {
    let mut map = SourceMap::new(canvas_width, canvas_height);
    let center_x = input_width as f32 / 2.0;
    let center_y = input_height as f32 / 2.0;
    for cy in 0..canvas_height as i32 {
        for cx in 0..canvas_width as i32 {
            let mut best: Option<(f32, u8)> = None;
            for (i, p) in positions.iter().enumerate() {
                let sx = cx - p.x;
                let sy = cy - p.y;
                if sx >= 0 && sy >= 0 && sx < input_width as i32 && sy < input_height as i32 {
                    let score = ((sx as f32 - center_x).powi(2) + (sy as f32 - center_y).powi(2))
                        .sqrt()
                        - i as f32 * 0.01;
                    if best.map(|b| score < b.0).unwrap_or(true) {
                        best = Some((score, i as u8));
                    }
                }
            }
            if let Some((_, source)) = best {
                map.set(cx as u32, cy as u32, source);
            }
        }
    }
    map
}

pub fn evaluate_stitch(
    stitched: &RgbImage,
    source_map: &SourceMap,
    thresholds: EvalThresholds,
) -> EvaluationReport {
    let (w, h) = stitched.dimensions();
    assert_eq!((w, h), (source_map.width, source_map.height));
    let gray = grayscale_vec(stitched);
    let grad = gradient_map(&gray, w, h);
    let (boundary, handoff_counts) = source_boundary_map(source_map);
    let local = box_filter(&grad, w, h, 10);

    let mut risk = vec![false; (w * h) as usize];
    let mut boundary_pixels = 0u32;
    let mut high_risk_boundary_pixels = 0u32;
    for idx in 0..risk.len() {
        if boundary[idx] {
            boundary_pixels += 1;
            if grad[idx] > 28.0 || grad[idx] - local[idx] > 18.0 {
                risk[idx] = true;
                high_risk_boundary_pixels += 1;
            }
        }
    }

    let components = risky_components(&risk, &grad, w, h);
    let largest = components.first().map(|c| c.area_px).unwrap_or(0);
    let duplicate_report = detect_duplicates(stitched);

    let mut handoff_pairs = Vec::new();
    for ((a, b), boundary_px) in handoff_counts {
        let risk_px = count_pair_risk(source_map, &risk, a, b);
        handoff_pairs.push(HandoffPairReport {
            sources: [a, b],
            boundary_px,
            risk_px,
        });
    }
    handoff_pairs.sort_by_key(|pair| Reverse(pair.risk_px));

    let mut failures = Vec::new();
    if high_risk_boundary_pixels > thresholds.max_high_risk_boundary_pixels {
        failures.push(format!(
            "high_risk_boundary_pixels {} > {}",
            high_risk_boundary_pixels, thresholds.max_high_risk_boundary_pixels
        ));
    }
    if largest > thresholds.max_largest_risky_component_area {
        failures.push(format!(
            "largest_risky_component_area {} > {}",
            largest, thresholds.max_largest_risky_component_area
        ));
    }
    if duplicate_report.band_duplicates.len() > thresholds.max_duplicate_bands {
        failures.push(format!(
            "duplicate_bands {} > {}",
            duplicate_report.band_duplicates.len(),
            thresholds.max_duplicate_bands
        ));
    }
    if duplicate_report.patch_duplicates.len() > thresholds.max_duplicate_patches {
        failures.push(format!(
            "duplicate_patches {} > {}",
            duplicate_report.patch_duplicates.len(),
            thresholds.max_duplicate_patches
        ));
    }

    EvaluationReport {
        passed: failures.is_empty(),
        image_size: [w, h],
        boundary_pixels,
        high_risk_boundary_pixels,
        largest_risky_component_area: largest,
        high_risk_components: components,
        handoff_pairs,
        duplicate_report,
        thresholds,
        failures,
    }
}

pub fn draw_evaluation_overlay(
    stitched: &RgbImage,
    source_map: &SourceMap,
    report: &EvaluationReport,
) -> RgbImage {
    let (w, h) = stitched.dimensions();
    let gray = grayscale_vec(stitched);
    let grad = gradient_map(&gray, w, h);
    let (boundary, _) = source_boundary_map(source_map);
    let local = box_filter(&grad, w, h, 10);
    let mut out = stitched.clone();
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) as usize;
            if boundary[idx] {
                out.put_pixel(x, y, Rgb([255, 230, 0]));
                if grad[idx] > 28.0 || grad[idx] - local[idx] > 18.0 {
                    draw_point_thick(&mut out, x, y, Rgb([255, 0, 0]), 2);
                }
            }
        }
    }
    for c in &report.high_risk_components {
        let [x, y, ww, hh] = c.bbox_xywh;
        if ww > 0 && hh > 0 {
            draw_rect(&mut out, x, y, x + ww - 1, y + hh - 1, Rgb([255, 0, 0]));
        }
    }
    out
}

pub fn draw_source_block_overlay(stitched: &RgbImage, source_map: &SourceMap) -> RgbImage {
    let colors = [
        Rgb([255, 70, 70]),
        Rgb([70, 160, 255]),
        Rgb([70, 255, 120]),
        Rgb([255, 220, 70]),
        Rgb([210, 90, 255]),
        Rgb([255, 130, 40]),
        Rgb([40, 255, 240]),
    ];
    let mut out = stitched.clone();
    for y in 0..stitched.height() {
        for x in 0..stitched.width() {
            let s = source_map.get(x, y);
            if s == SourceMap::UNASSIGNED {
                continue;
            }
            let c = colors[s as usize % colors.len()].0;
            let p = out.get_pixel_mut(x, y);
            p.0 = [
                ((p.0[0] as u16 * 70 + c[0] as u16 * 30) / 100) as u8,
                ((p.0[1] as u16 * 70 + c[1] as u16 * 30) / 100) as u8,
                ((p.0[2] as u16 * 70 + c[2] as u16 * 30) / 100) as u8,
            ];
        }
    }
    let (boundary, _) = source_boundary_map(source_map);
    for y in 0..stitched.height() {
        for x in 0..stitched.width() {
            if boundary[(y * stitched.width() + x) as usize] {
                out.put_pixel(x, y, Rgb([255, 255, 255]));
            }
        }
    }
    out
}

fn grayscale_vec(img: &RgbImage) -> Vec<f32> {
    img.pixels()
        .map(|p| 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32)
        .collect()
}

fn gradient_map(gray: &[f32], w: u32, h: u32) -> Vec<f32> {
    let mut grad = vec![0.0f32; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) as usize;
            if x > 0 {
                grad[idx] = grad[idx].max((gray[idx] - gray[(y * w + x - 1) as usize]).abs());
            }
            if y > 0 {
                grad[idx] = grad[idx].max((gray[idx] - gray[((y - 1) * w + x) as usize]).abs());
            }
        }
    }
    grad
}

type SourcePair = (u8, u8);
type SourcePairCount = (SourcePair, u32);

fn source_boundary_map(source_map: &SourceMap) -> (Vec<bool>, Vec<SourcePairCount>) {
    let w = source_map.width;
    let h = source_map.height;
    let mut boundary = vec![false; (w * h) as usize];
    let mut counts: Vec<SourcePairCount> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let s = source_map.get(x, y);
            if x + 1 < w {
                let t = source_map.get(x + 1, y);
                if s != t && s != SourceMap::UNASSIGNED && t != SourceMap::UNASSIGNED {
                    boundary[(y * w + x) as usize] = true;
                    boundary[(y * w + x + 1) as usize] = true;
                    add_pair_count(&mut counts, s, t);
                }
            }
            if y + 1 < h {
                let t = source_map.get(x, y + 1);
                if s != t && s != SourceMap::UNASSIGNED && t != SourceMap::UNASSIGNED {
                    boundary[(y * w + x) as usize] = true;
                    boundary[((y + 1) * w + x) as usize] = true;
                    add_pair_count(&mut counts, s, t);
                }
            }
        }
    }
    (boundary, counts)
}

fn add_pair_count(counts: &mut Vec<SourcePairCount>, a: u8, b: u8) {
    if a == SourceMap::UNASSIGNED || b == SourceMap::UNASSIGNED || a == b {
        return;
    }
    let pair = if a < b { (a, b) } else { (b, a) };
    if let Some((_, count)) = counts.iter_mut().find(|(p, _)| *p == pair) {
        *count += 1;
    } else {
        counts.push((pair, 1));
    }
}

fn box_filter(values: &[f32], w: u32, h: u32, radius: i32) -> Vec<f32> {
    let ww = w as usize;
    let hh = h as usize;
    let mut integral = vec![0.0f64; (ww + 1) * (hh + 1)];
    for y in 0..hh {
        let mut row_sum = 0.0f64;
        for x in 0..ww {
            row_sum += values[y * ww + x] as f64;
            integral[(y + 1) * (ww + 1) + x + 1] = integral[y * (ww + 1) + x + 1] + row_sum;
        }
    }

    let mut out = vec![0.0; values.len()];
    for y in 0..hh {
        for x in 0..ww {
            let x0 = x.saturating_sub(radius as usize);
            let y0 = y.saturating_sub(radius as usize);
            let x1 = min(ww - 1, x + radius as usize);
            let y1 = min(hh - 1, y + radius as usize);
            let xa = x0;
            let xb = x1 + 1;
            let ya = y0;
            let yb = y1 + 1;
            let sum = integral[yb * (ww + 1) + xb]
                - integral[ya * (ww + 1) + xb]
                - integral[yb * (ww + 1) + xa]
                + integral[ya * (ww + 1) + xa];
            let n = ((x1 - x0 + 1) * (y1 - y0 + 1)) as f64;
            out[y * ww + x] = (sum / n) as f32;
        }
    }
    out
}

fn risky_components(risk: &[bool], grad: &[f32], w: u32, h: u32) -> Vec<RiskyComponent> {
    let mut seen = vec![false; risk.len()];
    let mut components = Vec::new();
    for y0 in 0..h {
        for x0 in 0..w {
            let idx0 = (y0 * w + x0) as usize;
            if !risk[idx0] || seen[idx0] {
                continue;
            }
            let mut q = VecDeque::from([(x0, y0)]);
            seen[idx0] = true;
            let mut area = 0u32;
            let mut min_x = x0;
            let mut max_x = x0;
            let mut min_y = y0;
            let mut max_y = y0;
            let mut sum_grad = 0.0;
            let mut max_grad = 0.0f32;
            while let Some((x, y)) = q.pop_front() {
                let idx = (y * w + x) as usize;
                area += 1;
                min_x = min(min_x, x);
                max_x = max(max_x, x);
                min_y = min(min_y, y);
                max_y = max(max_y, y);
                sum_grad += grad[idx];
                max_grad = max_grad.max(grad[idx]);
                for (nx, ny) in neighbors4(x, y, w, h) {
                    let nidx = (ny * w + nx) as usize;
                    if risk[nidx] && !seen[nidx] {
                        seen[nidx] = true;
                        q.push_back((nx, ny));
                    }
                }
            }
            if area >= 40 {
                components.push(RiskyComponent {
                    area_px: area,
                    bbox_xywh: [min_x, min_y, max_x - min_x + 1, max_y - min_y + 1],
                    mean_gradient: sum_grad / area as f32,
                    max_gradient: max_grad,
                });
            }
        }
    }
    components.sort_by_key(|component| Reverse(component.area_px));
    components
}

fn count_pair_risk(source_map: &SourceMap, risk: &[bool], a: u8, b: u8) -> u32 {
    let w = source_map.width;
    let h = source_map.height;
    let mut count = 0;
    for y in 0..h {
        for x in 0..w {
            let s = source_map.get(x, y);
            if x + 1 < w {
                let t = source_map.get(x + 1, y);
                if pair_eq(s, t, a, b)
                    && (risk[(y * w + x) as usize] || risk[(y * w + x + 1) as usize])
                {
                    count += 1;
                }
            }
            if y + 1 < h {
                let t = source_map.get(x, y + 1);
                if pair_eq(s, t, a, b)
                    && (risk[(y * w + x) as usize] || risk[((y + 1) * w + x) as usize])
                {
                    count += 1;
                }
            }
        }
    }
    count
}

fn pair_eq(s: u8, t: u8, a: u8, b: u8) -> bool {
    (s == a && t == b) || (s == b && t == a)
}

fn neighbors4(x: u32, y: u32, w: u32, h: u32) -> impl Iterator<Item = (u32, u32)> {
    let mut v = Vec::with_capacity(4);
    if x > 0 {
        v.push((x - 1, y));
    }
    if y > 0 {
        v.push((x, y - 1));
    }
    if x + 1 < w {
        v.push((x + 1, y));
    }
    if y + 1 < h {
        v.push((x, y + 1));
    }
    v.into_iter()
}

fn draw_point_thick(img: &mut RgbImage, x: u32, y: u32, color: Rgb<u8>, radius: u32) {
    let x0 = x.saturating_sub(radius);
    let y0 = y.saturating_sub(radius);
    let x1 = min(img.width() - 1, x + radius);
    let y1 = min(img.height() - 1, y + radius);
    for yy in y0..=y1 {
        for xx in x0..=x1 {
            img.put_pixel(xx, yy, color);
        }
    }
}

pub fn detect_duplicates(img: &RgbImage) -> DuplicateReport {
    let bands = detect_duplicate_bands(img);
    let patches = detect_duplicate_patches(img);
    DuplicateReport {
        passed: bands.is_empty() && patches.is_empty(),
        band_duplicates: bands,
        patch_duplicates: patches,
    }
}

fn detect_duplicate_bands(img: &RgbImage) -> Vec<DuplicateBand> {
    let (w, h) = img.dimensions();
    let band_h = min(180, h / 3).max(80);
    let stride = 10;
    let small_w = 384;
    let ys: Vec<u32> = (0..=h.saturating_sub(band_h)).step_by(stride).collect();
    let feats: Vec<Vec<f32>> = ys
        .iter()
        .map(|&y| feature_for_rect(img, 0, y, w, band_h, small_w, band_h / 4))
        .collect();
    let mut candidates = Vec::new();
    for i in 0..ys.len() {
        for j in i + 1..ys.len() {
            let sep = ys[j] - ys[i];
            if sep < 300 {
                continue;
            }
            let s = dot(&feats[i], &feats[j]);
            let mad = avg_abs_diff_rect(img, [0, ys[i], w, band_h], [0, ys[j], w, band_h], 384, 45);
            if s >= 0.965 && mad < 15.0 {
                candidates.push(DuplicateBand {
                    score: s,
                    y1: ys[i],
                    y2: ys[j],
                    height: band_h,
                });
            }
        }
    }
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut selected: Vec<DuplicateBand> = Vec::new();
    for c in candidates {
        if selected.iter().all(|s| {
            (c.y1 as i32 - s.y1 as i32).abs() > band_h as i32 / 2
                || (c.y2 as i32 - s.y2 as i32).abs() > band_h as i32 / 2
        }) {
            selected.push(c);
        }
        if selected.len() >= 12 {
            break;
        }
    }
    selected
}

fn detect_duplicate_patches(img: &RgbImage) -> Vec<DuplicatePatch> {
    let (w, h) = img.dimensions();
    let patch_w = min(360, w / 2).max(120);
    let patch_h = min(240, h / 3).max(100);
    let x_stride = 90usize;
    let y_stride = 40usize;
    let xs: Vec<u32> = (0..=w.saturating_sub(patch_w)).step_by(x_stride).collect();
    let ys: Vec<u32> = (0..=h.saturating_sub(patch_h)).step_by(y_stride).collect();
    let mut patches: Vec<(u32, u32, Vec<f32>)> = Vec::new();
    for y in ys {
        for &x in &xs {
            let feat = feature_for_rect(img, x, y, patch_w, patch_h, 72, 48);
            if feature_energy(&feat) > 0.0001 {
                patches.push((x, y, feat));
            }
        }
    }
    let mut candidates = Vec::new();
    for i in 0..patches.len() {
        let (x1, y1, ref f1) = patches[i];
        for (x2, y2, f2) in patches.iter().skip(i + 1) {
            if y1.abs_diff(*y2) < 300 || x1.abs_diff(*x2) > 120 {
                continue;
            }
            let s = dot(f1, f2);
            let rect_a = [x1, y1, patch_w, patch_h];
            let rect_b = [*x2, *y2, patch_w, patch_h];
            let mad = avg_abs_diff_rect(img, rect_a, rect_b, 72, 48);
            if s >= 0.972 && mad < 15.0 {
                candidates.push(DuplicatePatch {
                    score: s,
                    rect_a,
                    rect_b,
                });
            }
        }
    }
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut selected: Vec<DuplicatePatch> = Vec::new();
    for c in candidates {
        if selected
            .iter()
            .all(|s| !patch_pair_close(&c, s, patch_w, patch_h))
        {
            selected.push(c);
        }
        if selected.len() >= 20 {
            break;
        }
    }
    selected
}

fn patch_pair_close(a: &DuplicatePatch, b: &DuplicatePatch, patch_w: u32, patch_h: u32) -> bool {
    let close = |r1: [u32; 4], r2: [u32; 4]| {
        r1[0].abs_diff(r2[0]) < patch_w / 2 && r1[1].abs_diff(r2[1]) < patch_h / 2
    };
    close(a.rect_a, b.rect_a) && close(a.rect_b, b.rect_b)
}

fn avg_abs_diff_rect(img: &RgbImage, a: [u32; 4], b: [u32; 4], out_w: u32, out_h: u32) -> f32 {
    let mut sum = 0.0f32;
    let mut n = 0.0f32;
    for oy in 0..out_h {
        for ox in 0..out_w {
            let ax = a[0] + ox * a[2] / out_w;
            let ay = a[1] + oy * a[3] / out_h;
            let bx = b[0] + ox * b[2] / out_w;
            let by = b[1] + oy * b[3] / out_h;
            let pa = img
                .get_pixel(ax.min(img.width() - 1), ay.min(img.height() - 1))
                .0;
            let pb = img
                .get_pixel(bx.min(img.width() - 1), by.min(img.height() - 1))
                .0;
            for c in 0..3 {
                sum += (pa[c] as f32 - pb[c] as f32).abs();
                n += 1.0;
            }
        }
    }
    sum / n.max(1.0)
}

fn feature_for_rect(
    img: &RgbImage,
    x0: u32,
    y0: u32,
    rw: u32,
    rh: u32,
    out_w: u32,
    out_h: u32,
) -> Vec<f32> {
    let mut gray = vec![0.0f32; (out_w * out_h) as usize];
    for oy in 0..out_h {
        for ox in 0..out_w {
            let sx0 = x0 + ox * rw / out_w;
            let sx1 = x0 + ((ox + 1) * rw / out_w).max(ox * rw / out_w + 1).min(rw);
            let sy0 = y0 + oy * rh / out_h;
            let sy1 = y0 + ((oy + 1) * rh / out_h).max(oy * rh / out_h + 1).min(rh);
            let mut sum = 0.0;
            let mut n = 0.0;
            for y in sy0..sy1 {
                for x in sx0..sx1 {
                    let p = img.get_pixel(x, y).0;
                    sum += 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32;
                    n += 1.0;
                }
            }
            gray[(oy * out_w + ox) as usize] = sum / n;
        }
    }
    let mut edge = vec![0.0f32; gray.len()];
    let ow = out_w as usize;
    let oh = out_h as usize;
    for y in 1..oh.saturating_sub(1) {
        for x in 1..ow.saturating_sub(1) {
            let gx = gray[y * ow + x + 1] - gray[y * ow + x - 1];
            let gy = gray[(y + 1) * ow + x] - gray[(y - 1) * ow + x];
            edge[y * ow + x] = (gx * gx + gy * gy).sqrt() * 0.9;
        }
    }
    gray.extend(edge);
    normalize_vec(gray)
}

fn normalize_vec(mut v: Vec<f32>) -> Vec<f32> {
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    for x in &mut v {
        *x -= mean;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
    for x in &mut v {
        *x /= norm;
    }
    v
}

fn feature_energy(v: &[f32]) -> f32 {
    v.iter().map(|x| x.abs()).sum::<f32>() / v.len() as f32
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

pub fn draw_duplicate_overlay(img: &RgbImage, report: &DuplicateReport) -> RgbImage {
    let mut out = img.clone();
    for d in &report.band_duplicates {
        draw_rect(
            &mut out,
            0,
            d.y1,
            img.width() - 1,
            d.y1 + d.height,
            Rgb([255, 0, 0]),
        );
        draw_rect(
            &mut out,
            0,
            d.y2,
            img.width() - 1,
            d.y2 + d.height,
            Rgb([255, 0, 0]),
        );
    }
    for d in &report.patch_duplicates {
        draw_rect(
            &mut out,
            d.rect_a[0],
            d.rect_a[1],
            d.rect_a[0] + d.rect_a[2],
            d.rect_a[1] + d.rect_a[3],
            Rgb([0, 255, 0]),
        );
        draw_rect(
            &mut out,
            d.rect_b[0],
            d.rect_b[1],
            d.rect_b[0] + d.rect_b[2],
            d.rect_b[1] + d.rect_b[3],
            Rgb([0, 255, 255]),
        );
    }
    out
}

fn draw_rect(img: &mut RgbImage, x0: u32, y0: u32, x1: u32, y1: u32, color: Rgb<u8>) {
    let x0 = min(x0, img.width() - 1);
    let x1 = min(x1, img.width() - 1);
    let y0 = min(y0, img.height() - 1);
    let y1 = min(y1, img.height() - 1);
    for t in 0..4u32 {
        for x in x0..=x1 {
            if y0 + t < img.height() {
                img.put_pixel(x, y0 + t, color);
            }
            if y1 >= t {
                img.put_pixel(x, y1 - t, color);
            }
        }
        for y in y0..=y1 {
            if x0 + t < img.width() {
                img.put_pixel(x0 + t, y, color);
            }
            if x1 >= t {
                img.put_pixel(x1 - t, y, color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn synthetic_canvas(w: u32, h: u32) -> RgbImage {
        let mut img = RgbImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let mut v = x.wrapping_mul(374_761_393) ^ y.wrapping_mul(668_265_263);
                v = (v ^ (v >> 13)).wrapping_mul(1_274_126_177);
                img.put_pixel(
                    x,
                    y,
                    Rgb([
                        (v & 0xff) as u8,
                        ((v >> 8) & 0xff) as u8,
                        ((v >> 16) & 0xff) as u8,
                    ]),
                );
            }
        }
        img
    }

    #[test]
    fn detects_horizontal_pan() {
        let canvas = synthetic_canvas(900, 300);
        let a = image::imageops::crop_imm(&canvas, 0, 0, 400, 300).to_image();
        let b = image::imageops::crop_imm(&canvas, 160, 0, 400, 300).to_image();
        let c = image::imageops::crop_imm(&canvas, 320, 0, 400, 300).to_image();
        let opts = StitchOptions {
            check_duplicates: false,
            min_shift_x: 40,
            min_shift_y: 40,
            ..Default::default()
        };
        let (_out, report) = stitch_images(&[a, b, c], &opts).unwrap();
        assert_eq!(report.shifts.len(), 2);
        assert!(
            report
                .shifts
                .iter()
                .all(|s| s.dx < -120 && s.dx > -200 && s.dy.abs() <= 4)
        );
        assert_eq!(report.canvas_width, 720);
    }

    #[test]
    fn detects_reverse_vertical_pan() {
        let canvas = synthetic_canvas(300, 900);
        let a = image::imageops::crop_imm(&canvas, 0, 0, 300, 400).to_image();
        let b = image::imageops::crop_imm(&canvas, 0, 150, 300, 400).to_image();
        let c = image::imageops::crop_imm(&canvas, 0, 300, 300, 400).to_image();
        let opts = StitchOptions {
            check_duplicates: false,
            min_shift_x: 40,
            min_shift_y: 40,
            ..Default::default()
        };
        let (_out, report) = stitch_images(&[a, b, c], &opts).unwrap();
        assert_eq!(report.shifts.len(), 2);
        assert!(
            report
                .shifts
                .iter()
                .all(|s| s.dy < -110 && s.dy > -190 && s.dx.abs() <= 4)
        );
        assert_eq!(report.canvas_height, 700);
    }

    #[test]
    fn eval_passes_clean_single_source_image() {
        let img = synthetic_canvas(220, 160);
        let mut map = SourceMap::new(220, 160);
        for y in 0..160 {
            for x in 0..220 {
                map.set(x, y, 0);
            }
        }
        let report = evaluate_stitch(&img, &map, EvalThresholds::default());
        assert!(
            report.passed,
            "unexpected eval failures: {:?}",
            report.failures
        );
        assert_eq!(report.boundary_pixels, 0);
    }

    #[test]
    fn eval_fails_visible_source_block_seam() {
        let mut img = RgbImage::new(220, 160);
        let mut map = SourceMap::new(220, 160);
        for y in 0..160 {
            for x in 0..220 {
                if x < 110 {
                    img.put_pixel(x, y, Rgb([0, 0, 0]));
                    map.set(x, y, 0);
                } else {
                    img.put_pixel(x, y, Rgb([255, 255, 255]));
                    map.set(x, y, 1);
                }
            }
        }
        let thresholds = EvalThresholds {
            max_high_risk_boundary_pixels: 0,
            max_largest_risky_component_area: 0,
            ..Default::default()
        };
        let report = evaluate_stitch(&img, &map, thresholds);
        assert!(!report.passed);
        assert!(report.high_risk_boundary_pixels > 0);
        assert!(report.largest_risky_component_area > 0);
    }

    #[test]
    fn source_map_round_trips_through_png_values() {
        let mut map = SourceMap::new(3, 2);
        map.set(0, 0, 0);
        map.set(1, 0, 1);
        map.set(2, 1, 6);
        let image = map.to_gray_image();
        let roundtrip = SourceMap::from_gray_image(&image);
        assert_eq!(roundtrip.get(0, 0), 0);
        assert_eq!(roundtrip.get(1, 0), 1);
        assert_eq!(roundtrip.get(2, 1), 6);
        assert_eq!(roundtrip.get(2, 0), SourceMap::UNASSIGNED);
    }
}
