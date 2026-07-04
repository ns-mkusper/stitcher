use anyhow::{Result, bail};
use image::{DynamicImage, ImageBuffer, Rgb, RgbImage};
use serde::Serialize;
use std::cmp::{max, min};
use std::path::Path;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum PanMode {
    Auto,
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Shift {
    /// Content displacement on screen from previous frame to this frame.
    /// Positive dx means content appears farther right in the later frame.
    /// Positive dy means content appears lower in the later frame.
    pub dx: i32,
    pub dy: i32,
    pub score: f32,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct Position {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct DuplicateBand {
    pub score: f32,
    pub y1: u32,
    pub y2: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct DuplicatePatch {
    pub score: f32,
    pub rect_a: [u32; 4],
    pub rect_b: [u32; 4],
}

#[derive(Debug, Clone, Serialize)]
pub struct DuplicateReport {
    pub passed: bool,
    pub band_duplicates: Vec<DuplicateBand>,
    pub patch_duplicates: Vec<DuplicatePatch>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StitchReport {
    pub shifts: Vec<Shift>,
    pub raw_positions: Vec<Position>,
    pub normalized_positions: Vec<Position>,
    pub canvas_width: u32,
    pub canvas_height: u32,
    pub duplicate_report: Option<DuplicateReport>,
}

#[derive(Debug, Clone)]
pub struct StitchOptions {
    pub mode: PanMode,
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
    let stitched = synthesize_nearest_center(images, &positions, canvas_w, canvas_h)?;
    let duplicate_report = opts.check_duplicates.then(|| detect_duplicates(&stitched));
    if let Some(report) = &duplicate_report
        && !report.passed
    {
        bail!("DUPLICATE TEST: FAIL");
    }

    Ok((
        stitched,
        StitchReport {
            shifts,
            raw_positions,
            normalized_positions: positions,
            canvas_width: canvas_w,
            canvas_height: canvas_h,
            duplicate_report,
        },
    ))
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
    let (w, h) = images[0].dimensions();
    let center_x = w as f32 / 2.0;
    let center_y = h as f32 / 2.0;
    let mut out = ImageBuffer::from_pixel(canvas_w, canvas_h, Rgb([0, 0, 0]));
    for cy in 0..canvas_h as i32 {
        for cx in 0..canvas_w as i32 {
            let mut best: Option<(f32, usize, u32, u32)> = None;
            for (i, p) in positions.iter().enumerate() {
                let sx = cx - p.x;
                let sy = cy - p.y;
                if sx >= 0 && sy >= 0 && sx < w as i32 && sy < h as i32 {
                    let score =
                        ((sx as f32 - center_x).powi(2) + (sy as f32 - center_y).powi(2)).sqrt();
                    if best.map(|b| score < b.0).unwrap_or(true) {
                        best = Some((score, i, sx as u32, sy as u32));
                    }
                }
            }
            if let Some((_score, i, sx, sy)) = best {
                out.put_pixel(cx as u32, cy as u32, *images[i].get_pixel(sx, sy));
            }
        }
    }
    Ok(out)
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
}
