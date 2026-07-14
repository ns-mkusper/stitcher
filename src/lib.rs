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
    #[serde(default)]
    pub local_warp: Option<LocalWarpReport>,
    pub duplicate_report: Option<DuplicateReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalWarpReport {
    pub strip_width: u32,
    pub max_dy: i32,
    pub protect_foreground: bool,
    pub shifted_pixels: u32,
    pub deltas_by_source: Vec<Vec<i32>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SourceSelection {
    NearestCenter,
    SeamDp,
    SeamDpMotion,
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
    pub monotonic_frame_filter: bool,
    pub foreground_masks: Option<Vec<ForegroundMask>>,
    pub foreground_mask_dilate: u32,
    pub foreground_mask_penalty: f32,
    pub local_warp: bool,
    pub local_warp_strip_width: u32,
    pub local_warp_max_dy: i32,
    pub local_warp_protect_foreground: bool,
    pub seam_motion_weight: f32,
    pub seam_motion_radius: u32,
    pub seam_motion_threshold: f32,
    pub seam_motion_mask_dilate: u32,
    pub seam_motion_hard_penalty: f32,
    pub seam_edge_weight: f32,
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
            monotonic_frame_filter: false,
            foreground_masks: None,
            foreground_mask_dilate: 0,
            foreground_mask_penalty: 0.0,
            local_warp: false,
            local_warp_strip_width: 128,
            local_warp_max_dy: 4,
            local_warp_protect_foreground: true,
            seam_motion_weight: 2.0,
            seam_motion_radius: 6,
            seam_motion_threshold: 12.0,
            seam_motion_mask_dilate: 0,
            seam_motion_hard_penalty: 0.0,
            seam_edge_weight: 0.15,
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

    if let Some(masks) = &opts.foreground_masks {
        if masks.len() != images.len() {
            bail!(
                "foreground mask count {} does not match input image count {}",
                masks.len(),
                images.len()
            );
        }
        for (idx, mask) in masks.iter().enumerate() {
            if (mask.width, mask.height) != (w, h) {
                bail!(
                    "foreground mask {} is {}x{}, expected {}x{}",
                    idx,
                    mask.width,
                    mask.height,
                    w,
                    h
                );
            }
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
    let active_sources = opts
        .monotonic_frame_filter
        .then(|| monotonic_active_sources(&raw_positions));
    let active_sources = active_sources.as_deref();
    let foreground_masks = opts.foreground_masks.as_deref();
    let foreground_dilate = opts.foreground_mask_dilate;
    let foreground_penalty = opts.foreground_mask_penalty;
    let (mut stitched, source_map) = match opts.source_selection {
        SourceSelection::NearestCenter => {
            synthesize_nearest_center_with_source_map(images, &positions, canvas_w, canvas_h)?
        }
        SourceSelection::SeamDp => synthesize_vertical_seams_with_source_map_opts(
            images,
            &positions,
            canvas_w,
            canvas_h,
            SeamOptions::BASIC.with_foreground_mask(foreground_dilate, foreground_penalty),
            active_sources,
            foreground_masks,
        )?,
        SourceSelection::SeamDpMotion => synthesize_vertical_seams_with_source_map_opts(
            images,
            &positions,
            canvas_w,
            canvas_h,
            SeamOptions::motion_aware(
                opts.seam_motion_weight,
                opts.seam_motion_radius,
                opts.seam_motion_threshold,
                opts.seam_motion_mask_dilate,
                opts.seam_motion_hard_penalty,
                opts.seam_edge_weight,
            )
            .with_foreground_mask(foreground_dilate, foreground_penalty),
            active_sources,
            foreground_masks,
        )?,
    };
    let local_warp = if opts.local_warp {
        let (warped, report) = render_local_warped_from_source_map(
            images,
            foreground_masks,
            &positions,
            &source_map,
            opts.local_warp_strip_width,
            opts.local_warp_max_dy,
            opts.local_warp_protect_foreground,
        )?;
        stitched = warped;
        Some(report)
    } else {
        None
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
        local_warp,
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

fn monotonic_active_sources(raw_positions: &[Position]) -> Vec<bool> {
    if raw_positions.len() <= 2 {
        return vec![true; raw_positions.len()];
    }
    let span_x = raw_positions.iter().map(|p| p.x).max().unwrap_or(0)
        - raw_positions.iter().map(|p| p.x).min().unwrap_or(0);
    let span_y = raw_positions.iter().map(|p| p.y).max().unwrap_or(0)
        - raw_positions.iter().map(|p| p.y).min().unwrap_or(0);
    let values: Vec<i32> = if span_y >= span_x {
        raw_positions.iter().map(|p| p.y).collect()
    } else {
        raw_positions.iter().map(|p| p.x).collect()
    };
    let inc = longest_monotonic_indices(&values, true, 8);
    let dec = longest_monotonic_indices(&values, false, 8);
    let prefer_increasing = values.last().unwrap_or(&0) >= values.first().unwrap_or(&0);
    let chosen = if dec.len() > inc.len() || (dec.len() == inc.len() && !prefer_increasing) {
        dec
    } else {
        inc
    };
    if chosen.len() < 2 || chosen.len() == raw_positions.len() {
        return vec![true; raw_positions.len()];
    }
    let mut active = vec![false; raw_positions.len()];
    for idx in chosen {
        active[idx] = true;
    }
    active
}

fn longest_monotonic_indices(values: &[i32], increasing: bool, tolerance: i32) -> Vec<usize> {
    let n = values.len();
    let mut dp = vec![1usize; n];
    let mut prev = vec![None; n];
    for i in 0..n {
        for j in 0..i {
            let monotonic = if increasing {
                values[i] + tolerance >= values[j]
            } else {
                values[i] <= values[j] + tolerance
            };
            if monotonic && dp[j] + 1 > dp[i] {
                dp[i] = dp[j] + 1;
                prev[i] = Some(j);
            }
        }
    }
    let mut best = (0..n).max_by_key(|&i| dp[i]).unwrap_or(0);
    let mut out = Vec::new();
    loop {
        out.push(best);
        if let Some(p) = prev[best] {
            best = p;
        } else {
            break;
        }
    }
    out.reverse();
    out
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

#[derive(Debug, Clone, Copy)]
struct SeamOptions {
    motion_weight: f32,
    motion_radius: u32,
    motion_threshold: f32,
    motion_mask_dilate: u32,
    motion_hard_penalty: f32,
    foreground_mask_dilate: u32,
    foreground_mask_penalty: f32,
    edge_weight: f32,
}

impl SeamOptions {
    const BASIC: Self = Self {
        motion_weight: 0.0,
        motion_radius: 0,
        motion_threshold: 0.0,
        motion_mask_dilate: 0,
        motion_hard_penalty: 0.0,
        foreground_mask_dilate: 0,
        foreground_mask_penalty: 0.0,
        edge_weight: 0.15,
    };

    fn motion_aware(
        motion_weight: f32,
        motion_radius: u32,
        motion_threshold: f32,
        motion_mask_dilate: u32,
        motion_hard_penalty: f32,
        edge_weight: f32,
    ) -> Self {
        Self {
            motion_weight,
            motion_radius,
            motion_threshold,
            motion_mask_dilate,
            motion_hard_penalty,
            foreground_mask_dilate: 0,
            foreground_mask_penalty: 0.0,
            edge_weight,
        }
    }

    fn with_foreground_mask(mut self, dilate: u32, penalty: f32) -> Self {
        self.foreground_mask_dilate = dilate;
        self.foreground_mask_penalty = penalty;
        self
    }
}

pub fn synthesize_vertical_seams_with_source_map(
    images: &[RgbImage],
    positions: &[Position],
    canvas_w: u32,
    canvas_h: u32,
) -> Result<(RgbImage, SourceMap)> {
    synthesize_vertical_seams_with_source_map_opts(
        images,
        positions,
        canvas_w,
        canvas_h,
        SeamOptions::BASIC,
        None,
        None,
    )
}

pub fn synthesize_vertical_seams_motion_with_source_map(
    images: &[RgbImage],
    positions: &[Position],
    canvas_w: u32,
    canvas_h: u32,
) -> Result<(RgbImage, SourceMap)> {
    synthesize_vertical_seams_with_source_map_opts(
        images,
        positions,
        canvas_w,
        canvas_h,
        SeamOptions::motion_aware(2.0, 6, 12.0, 0, 0.0, 0.15),
        None,
        None,
    )
}

fn synthesize_vertical_seams_with_source_map_opts(
    images: &[RgbImage],
    positions: &[Position],
    canvas_w: u32,
    canvas_h: u32,
    seam_options: SeamOptions,
    active_sources: Option<&[bool]>,
    foreground_masks: Option<&[ForegroundMask]>,
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

    let mut order: Vec<usize> = (0..images.len())
        .filter(|&i| active_sources.is_none_or(|active| active[i]))
        .collect();
    if order.is_empty() {
        order = (0..images.len()).collect();
    }
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
            let seam = find_vertical_pan_seam(
                &canvas,
                &source_map,
                images,
                positions,
                idx,
                x0,
                y0,
                x1,
                y1,
                seam_options,
                foreground_masks,
            );
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

    if let Some(active) = active_sources {
        for (idx, img) in images.iter().enumerate() {
            if !active[idx] {
                paste_unassigned(&mut canvas, &mut source_map, img, positions[idx], idx as u8);
            }
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

fn paste_unassigned(
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
            if cx >= 0
                && cy >= 0
                && cx < canvas.width() as i32
                && cy < canvas.height() as i32
                && source_map.get(cx as u32, cy as u32) == SourceMap::UNASSIGNED
            {
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

#[allow(clippy::too_many_arguments)]
fn find_vertical_pan_seam(
    canvas: &RgbImage,
    source_map: &SourceMap,
    images: &[RgbImage],
    positions: &[Position],
    incoming_idx: usize,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    seam_options: SeamOptions,
    foreground_masks: Option<&[ForegroundMask]>,
) -> Vec<u32> {
    let incoming = &images[incoming_idx];
    let pos = positions[incoming_idx];
    let ow = (x1 - x0) as usize;
    let oh = (y1 - y0) as usize;
    if ow == 0 || oh == 0 {
        return Vec::new();
    }
    let hard_motion_mask = if seam_options.motion_hard_penalty > 0.0 {
        motion_mask_for_overlap(
            source_map,
            images,
            positions,
            incoming_idx,
            x0,
            y0,
            ow,
            oh,
            seam_options.motion_radius,
            seam_options.motion_threshold,
            seam_options.motion_mask_dilate,
        )
    } else {
        vec![false; ow * oh]
    };
    let foreground_mask = if seam_options.foreground_mask_penalty > 0.0 {
        foreground_mask_for_overlap(
            source_map,
            foreground_masks,
            positions,
            incoming_idx,
            x0,
            y0,
            ow,
            oh,
            seam_options.foreground_mask_dilate,
        )
    } else {
        vec![false; ow * oh]
    };
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
            let motion = if seam_options.motion_weight > 0.0 {
                let existing_source = source_map.get(cx, cy);
                let diff = aligned_temporal_diff(
                    images,
                    positions,
                    existing_source,
                    incoming_idx,
                    cx,
                    cy,
                    seam_options.motion_radius,
                );
                (diff - seam_options.motion_threshold).max(0.0)
            } else {
                0.0
            };
            let hard_motion = if hard_motion_mask[yy * ow + xx] {
                seam_options.motion_hard_penalty
            } else {
                0.0
            };
            let foreground_penalty = if foreground_mask[yy * ow + xx] {
                seam_options.foreground_mask_penalty
            } else {
                0.0
            };
            cost[yy * ow + xx] = color
                + seam_options.edge_weight * edge
                + edge_margin_penalty
                + seam_options.motion_weight * motion
                + hard_motion
                + foreground_penalty;
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
            -1 => y.saturating_sub(1),
            1 => (y + 1).min(oh - 1),
            _ => y,
        };
        seam[x - 1] = y0 + y as u32;
    }
    seam
}

#[allow(clippy::too_many_arguments)]
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

#[allow(clippy::too_many_arguments)]
fn foreground_mask_for_overlap(
    source_map: &SourceMap,
    masks: Option<&[ForegroundMask]>,
    positions: &[Position],
    incoming_idx: usize,
    x0: u32,
    y0: u32,
    ow: usize,
    oh: usize,
    dilate: u32,
) -> Vec<bool> {
    let Some(masks) = masks else {
        return vec![false; ow * oh];
    };
    if incoming_idx >= masks.len() {
        return vec![false; ow * oh];
    }
    let mut mask = vec![false; ow * oh];
    for yy in 0..oh {
        for xx in 0..ow {
            let cx = x0 + xx as u32;
            let cy = y0 + yy as u32;
            let incoming_hit =
                mask_covers_canvas_pixel(&masks[incoming_idx], positions[incoming_idx], cx, cy);
            let existing_source = source_map.get(cx, cy);
            let existing_hit = existing_source != SourceMap::UNASSIGNED
                && (existing_source as usize) < masks.len()
                && mask_covers_canvas_pixel(
                    &masks[existing_source as usize],
                    positions[existing_source as usize],
                    cx,
                    cy,
                );
            if incoming_hit || existing_hit {
                mask[yy * ow + xx] = true;
            }
        }
    }
    dilate_bool_mask(&mask, ow, oh, dilate as usize)
}

fn mask_covers_canvas_pixel(mask: &ForegroundMask, pos: Position, cx: u32, cy: u32) -> bool {
    let sx = cx as i32 - pos.x;
    let sy = cy as i32 - pos.y;
    if sx < 0 || sy < 0 || sx >= mask.width as i32 || sy >= mask.height as i32 {
        return false;
    }
    mask.get(sx as u32, sy as u32)
}

#[allow(clippy::too_many_arguments)]
fn motion_mask_for_overlap(
    source_map: &SourceMap,
    images: &[RgbImage],
    positions: &[Position],
    incoming_idx: usize,
    x0: u32,
    y0: u32,
    ow: usize,
    oh: usize,
    radius: u32,
    threshold: f32,
    dilate: u32,
) -> Vec<bool> {
    let mut mask = vec![false; ow * oh];
    for yy in 0..oh {
        for xx in 0..ow {
            let cx = x0 + xx as u32;
            let cy = y0 + yy as u32;
            let existing_source = source_map.get(cx, cy);
            if existing_source == SourceMap::UNASSIGNED {
                continue;
            }
            let adjacent_diff = aligned_temporal_diff(
                images,
                positions,
                existing_source,
                incoming_idx,
                cx,
                cy,
                radius,
            );
            let temporal_diff = covering_temporal_disagreement(images, positions, cx, cy, radius);
            let diff = adjacent_diff.max(temporal_diff);
            if diff > threshold {
                mask[yy * ow + xx] = true;
            }
        }
    }
    dilate_bool_mask(&mask, ow, oh, dilate as usize)
}

fn dilate_bool_mask(mask: &[bool], w: usize, h: usize, radius: usize) -> Vec<bool> {
    if radius == 0 || mask.is_empty() {
        return mask.to_vec();
    }
    let mut out = mask.to_vec();
    for y in 0..h {
        for x in 0..w {
            if !mask[y * w + x] {
                continue;
            }
            let y0 = y.saturating_sub(radius);
            let y1 = min(h - 1, y + radius);
            let x0 = x.saturating_sub(radius);
            let x1 = min(w - 1, x + radius);
            for yy in y0..=y1 {
                for xx in x0..=x1 {
                    out[yy * w + xx] = true;
                }
            }
        }
    }
    out
}

fn covering_temporal_disagreement(
    images: &[RgbImage],
    positions: &[Position],
    cx: u32,
    cy: u32,
    radius: u32,
) -> f32 {
    let radius = radius as i32;
    let mut total = 0.0;
    let mut count = 0u32;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let sample_x = cx as i32 + dx;
            let sample_y = cy as i32 + dy;
            let mut min_rgb = [u8::MAX; 3];
            let mut max_rgb = [u8::MIN; 3];
            let mut covered = 0u32;
            for (idx, img) in images.iter().enumerate() {
                let sx = sample_x - positions[idx].x;
                let sy = sample_y - positions[idx].y;
                if sx < 0 || sy < 0 || sx >= img.width() as i32 || sy >= img.height() as i32 {
                    continue;
                }
                let p = img.get_pixel(sx as u32, sy as u32).0;
                for c in 0..3 {
                    min_rgb[c] = min_rgb[c].min(p[c]);
                    max_rgb[c] = max_rgb[c].max(p[c]);
                }
                covered += 1;
            }
            if covered >= 2 {
                total += ((max_rgb[0] as f32 - min_rgb[0] as f32).abs()
                    + (max_rgb[1] as f32 - min_rgb[1] as f32).abs()
                    + (max_rgb[2] as f32 - min_rgb[2] as f32).abs())
                    / 3.0;
                count += 1;
            }
        }
    }
    if count == 0 {
        0.0
    } else {
        total / count as f32
    }
}

fn aligned_temporal_diff(
    images: &[RgbImage],
    positions: &[Position],
    existing_source: u8,
    incoming_idx: usize,
    cx: u32,
    cy: u32,
    radius: u32,
) -> f32 {
    if existing_source == SourceMap::UNASSIGNED {
        return 0.0;
    }
    let existing_idx = existing_source as usize;
    if existing_idx >= images.len() || incoming_idx >= images.len() {
        return 0.0;
    }

    let existing_pos = positions[existing_idx];
    let incoming_pos = positions[incoming_idx];
    let radius = radius as i32;
    let mut total = 0.0;
    let mut count = 0u32;

    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let sample_x = cx as i32 + dx;
            let sample_y = cy as i32 + dy;
            let ex = sample_x - existing_pos.x;
            let ey = sample_y - existing_pos.y;
            let ix = sample_x - incoming_pos.x;
            let iy = sample_y - incoming_pos.y;
            if ex < 0
                || ey < 0
                || ix < 0
                || iy < 0
                || ex >= images[existing_idx].width() as i32
                || ey >= images[existing_idx].height() as i32
                || ix >= images[incoming_idx].width() as i32
                || iy >= images[incoming_idx].height() as i32
            {
                continue;
            }
            total += rgb_abs_diff(
                images[existing_idx].get_pixel(ex as u32, ey as u32).0,
                images[incoming_idx].get_pixel(ix as u32, iy as u32).0,
            );
            count += 1;
        }
    }

    if count == 0 {
        0.0
    } else {
        total / count as f32
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

#[allow(clippy::too_many_arguments)]
fn render_local_warped_from_source_map(
    images: &[RgbImage],
    foreground_masks: Option<&[ForegroundMask]>,
    positions: &[Position],
    source_map: &SourceMap,
    strip_width: u32,
    max_dy: i32,
    protect_foreground: bool,
) -> Result<(RgbImage, LocalWarpReport)> {
    if images.is_empty() {
        bail!("need at least one image");
    }
    let strip_width = strip_width.max(1);
    let strip_count = source_map.width.div_ceil(strip_width) as usize;
    let mut deltas_by_source = vec![vec![0i32; strip_count]; images.len()];
    for (source_idx, source_deltas) in deltas_by_source.iter_mut().enumerate() {
        for (strip, delta) in source_deltas.iter_mut().enumerate() {
            *delta = estimate_local_warp_delta(
                images,
                foreground_masks,
                positions,
                source_idx,
                source_map,
                strip as u32 * strip_width,
                ((strip as u32 + 1) * strip_width).min(source_map.width),
                max_dy,
            );
        }
        let original = source_deltas.clone();
        for (strip, delta) in source_deltas.iter_mut().enumerate() {
            let start = strip.saturating_sub(1);
            let end = (strip + 2).min(strip_count);
            let mut vals = original[start..end].to_vec();
            vals.sort_unstable();
            *delta = vals[vals.len() / 2];
        }
    }

    let mut out = ImageBuffer::from_pixel(source_map.width, source_map.height, Rgb([0, 0, 0]));
    let mut shifted_pixels = 0u32;
    for y in 0..source_map.height {
        for x in 0..source_map.width {
            let source = source_map.get(x, y);
            if source == SourceMap::UNASSIGNED {
                continue;
            }
            let source_idx = source as usize;
            if source_idx >= images.len() {
                continue;
            }
            let strip = (x / strip_width).min(strip_count as u32 - 1) as usize;
            let mut delta = deltas_by_source[source_idx][strip];
            if protect_foreground
                && foreground_masks.is_some_and(|masks| {
                    source_idx < masks.len()
                        && mask_covers_canvas_pixel(&masks[source_idx], positions[source_idx], x, y)
                })
            {
                delta = 0;
            }
            let sx = x as i32 - positions[source_idx].x;
            let mut sy = y as i32 - positions[source_idx].y + delta;
            if sx < 0 || sx >= images[source_idx].width() as i32 {
                continue;
            }
            if sy < 0 || sy >= images[source_idx].height() as i32 {
                sy = y as i32 - positions[source_idx].y;
                delta = 0;
            }
            if sy >= 0 && sy < images[source_idx].height() as i32 {
                out.put_pixel(x, y, *images[source_idx].get_pixel(sx as u32, sy as u32));
                if delta != 0 {
                    shifted_pixels += 1;
                }
            }
        }
    }
    Ok((
        out,
        LocalWarpReport {
            strip_width,
            max_dy,
            protect_foreground,
            shifted_pixels,
            deltas_by_source,
        },
    ))
}

#[allow(clippy::too_many_arguments)]
fn estimate_local_warp_delta(
    images: &[RgbImage],
    foreground_masks: Option<&[ForegroundMask]>,
    positions: &[Position],
    source_idx: usize,
    source_map: &SourceMap,
    x0: u32,
    x1: u32,
    max_dy: i32,
) -> i32 {
    let mut best = (f32::INFINITY, 0i32);
    for delta in -max_dy..=max_dy {
        let mut total = 0.0f32;
        let mut count = 0u32;
        for y in (0..source_map.height).step_by(12) {
            for x in (x0..x1).step_by(24) {
                let source = source_map.get(x, y);
                if source as usize != source_idx {
                    continue;
                }
                if foreground_masks.is_some_and(|masks| {
                    source_idx < masks.len()
                        && mask_covers_canvas_pixel(&masks[source_idx], positions[source_idx], x, y)
                }) {
                    continue;
                }
                let sx = x as i32 - positions[source_idx].x;
                let sy = y as i32 - positions[source_idx].y + delta;
                if sx < 0
                    || sy < 0
                    || sx >= images[source_idx].width() as i32
                    || sy >= images[source_idx].height() as i32
                {
                    continue;
                }
                let mut reference: Option<[u8; 3]> = None;
                for (other_idx, other) in images.iter().enumerate() {
                    if other_idx == source_idx {
                        continue;
                    }
                    if foreground_masks.is_some_and(|masks| {
                        other_idx < masks.len()
                            && mask_covers_canvas_pixel(
                                &masks[other_idx],
                                positions[other_idx],
                                x,
                                y,
                            )
                    }) {
                        continue;
                    }
                    let ox = x as i32 - positions[other_idx].x;
                    let oy = y as i32 - positions[other_idx].y;
                    if ox >= 0 && oy >= 0 && ox < other.width() as i32 && oy < other.height() as i32
                    {
                        reference = Some(other.get_pixel(ox as u32, oy as u32).0);
                        break;
                    }
                }
                if let Some(reference) = reference {
                    total += rgb_abs_diff(
                        images[source_idx].get_pixel(sx as u32, sy as u32).0,
                        reference,
                    );
                    count += 1;
                }
            }
        }
        if count >= 20 {
            let score = total / count as f32 + delta.unsigned_abs() as f32 * 0.1;
            if score < best.0 {
                best = (score, delta);
            }
        }
    }
    best.1
}

#[derive(Debug, Clone)]
pub struct SourceCoordinateMap {
    pub width: u32,
    pub height: u32,
    pub sources: Vec<u8>,
    pub source_x: Vec<u16>,
    pub source_y: Vec<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceCoordinateVerification {
    pub checked_pixels: u32,
    pub mismatched_pixels: u32,
    pub out_of_bounds_pixels: u32,
    pub unassigned_pixels: u32,
}

impl SourceCoordinateMap {
    pub const UNASSIGNED: u8 = u8::MAX;

    pub fn new(width: u32, height: u32) -> Self {
        let len = (width * height) as usize;
        Self {
            width,
            height,
            sources: vec![Self::UNASSIGNED; len],
            source_x: vec![0; len],
            source_y: vec![0; len],
        }
    }

    pub fn get(&self, x: u32, y: u32) -> Option<(u8, u16, u16)> {
        let idx = (y * self.width + x) as usize;
        let source = self.sources[idx];
        (source != Self::UNASSIGNED).then_some((source, self.source_x[idx], self.source_y[idx]))
    }

    pub fn set(&mut self, x: u32, y: u32, source: u8, sx: u16, sy: u16) {
        let idx = (y * self.width + x) as usize;
        self.sources[idx] = source;
        self.source_x[idx] = sx;
        self.source_y[idx] = sy;
    }

    pub fn from_source_map(source_map: &SourceMap, positions: &[Position]) -> Self {
        let mut map = Self::new(source_map.width, source_map.height);
        for y in 0..source_map.height {
            for x in 0..source_map.width {
                let source = source_map.get(x, y);
                if source == SourceMap::UNASSIGNED {
                    continue;
                }
                let source_idx = source as usize;
                if source_idx >= positions.len() {
                    continue;
                }
                let sx = x as i32 - positions[source_idx].x;
                let sy = y as i32 - positions[source_idx].y;
                if sx >= 0 && sy >= 0 && sx <= u16::MAX as i32 && sy <= u16::MAX as i32 {
                    map.set(x, y, source, sx as u16, sy as u16);
                }
            }
        }
        map
    }

    pub fn from_source_map_with_local_warp(
        source_map: &SourceMap,
        positions: &[Position],
        local_warp: Option<&LocalWarpReport>,
        foreground_masks: Option<&[ForegroundMask]>,
    ) -> Self {
        let mut map = Self::new(source_map.width, source_map.height);
        for y in 0..source_map.height {
            for x in 0..source_map.width {
                let source = source_map.get(x, y);
                if source == SourceMap::UNASSIGNED {
                    continue;
                }
                let source_idx = source as usize;
                if source_idx >= positions.len() {
                    continue;
                }
                let sx = x as i32 - positions[source_idx].x;
                let mut sy = y as i32 - positions[source_idx].y;
                if let Some(warp) = local_warp
                    && source_idx < warp.deltas_by_source.len()
                    && warp.strip_width > 0
                {
                    let strip = ((x / warp.strip_width) as usize)
                        .min(warp.deltas_by_source[source_idx].len().saturating_sub(1));
                    let mut delta = warp.deltas_by_source[source_idx]
                        .get(strip)
                        .copied()
                        .unwrap_or(0);
                    if warp.protect_foreground
                        && foreground_masks.is_some_and(|masks| {
                            source_idx < masks.len()
                                && mask_covers_canvas_pixel(
                                    &masks[source_idx],
                                    positions[source_idx],
                                    x,
                                    y,
                                )
                        })
                    {
                        delta = 0;
                    }
                    sy += delta;
                }
                if sx >= 0 && sy >= 0 && sx <= u16::MAX as i32 && sy <= u16::MAX as i32 {
                    map.set(x, y, source, sx as u16, sy as u16);
                }
            }
        }
        map
    }

    pub fn to_rgb16_image(&self) -> ImageBuffer<Rgb<u16>, Vec<u16>> {
        let mut img = ImageBuffer::from_pixel(self.width, self.height, Rgb([0u16, 0u16, 0u16]));
        for y in 0..self.height {
            for x in 0..self.width {
                let idx = (y * self.width + x) as usize;
                let source = self.sources[idx];
                if source != Self::UNASSIGNED {
                    img.put_pixel(
                        x,
                        y,
                        Rgb([source as u16 + 1, self.source_x[idx], self.source_y[idx]]),
                    );
                }
            }
        }
        img
    }

    pub fn from_rgb16_image(img: &ImageBuffer<Rgb<u16>, Vec<u16>>) -> Self {
        let (width, height) = img.dimensions();
        let mut map = Self::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let p = img.get_pixel(x, y).0;
                if p[0] != 0 {
                    map.set(x, y, (p[0] - 1) as u8, p[1], p[2]);
                }
            }
        }
        map
    }
}

pub fn verify_source_coordinate_map(
    stitched: &RgbImage,
    inputs: &[RgbImage],
    coord_map: &SourceCoordinateMap,
) -> SourceCoordinateVerification {
    assert_eq!(
        (stitched.width(), stitched.height()),
        (coord_map.width, coord_map.height)
    );
    let mut report = SourceCoordinateVerification {
        checked_pixels: 0,
        mismatched_pixels: 0,
        out_of_bounds_pixels: 0,
        unassigned_pixels: 0,
    };
    for y in 0..coord_map.height {
        for x in 0..coord_map.width {
            let Some((source, sx, sy)) = coord_map.get(x, y) else {
                report.unassigned_pixels += 1;
                continue;
            };
            let source_idx = source as usize;
            if source_idx >= inputs.len()
                || sx as u32 >= inputs[source_idx].width()
                || sy as u32 >= inputs[source_idx].height()
            {
                report.out_of_bounds_pixels += 1;
                continue;
            }
            report.checked_pixels += 1;
            if stitched.get_pixel(x, y) != inputs[source_idx].get_pixel(sx as u32, sy as u32) {
                report.mismatched_pixels += 1;
            }
        }
    }
    report
}

#[derive(Debug, Clone)]
pub struct ForegroundMask {
    pub width: u32,
    pub height: u32,
    pub data: Vec<bool>,
}

impl ForegroundMask {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![false; (width * height) as usize],
        }
    }

    pub fn get(&self, x: u32, y: u32) -> bool {
        self.data[(y * self.width + x) as usize]
    }

    pub fn set(&mut self, x: u32, y: u32, value: bool) {
        self.data[(y * self.width + x) as usize] = value;
    }

    pub fn from_gray_image(img: &GrayImage, threshold: u8) -> Self {
        let (width, height) = img.dimensions();
        let mut mask = Self::new(width, height);
        for y in 0..height {
            for x in 0..width {
                mask.set(x, y, img.get_pixel(x, y).0[0] > threshold);
            }
        }
        mask
    }

    pub fn to_gray_image(&self) -> GrayImage {
        let mut img = GrayImage::new(self.width, self.height);
        for y in 0..self.height {
            for x in 0..self.width {
                img.put_pixel(x, y, Luma([if self.get(x, y) { 255 } else { 0 }]));
            }
        }
        img
    }
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
    /// Minimum average luminance-gradient energy. Set to 0 to disable.
    pub min_mean_gradient: f32,
    /// Minimum 95th percentile luminance-gradient energy. Set to 0 to disable.
    pub min_p95_gradient: f32,
}

impl Default for EvalThresholds {
    fn default() -> Self {
        Self {
            max_high_risk_boundary_pixels: 250,
            max_largest_risky_component_area: 300,
            max_duplicate_bands: 0,
            max_duplicate_patches: 0,
            min_mean_gradient: 0.0,
            min_p95_gradient: 0.0,
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
    pub source_map_distinct_sources: usize,
    pub source_coord_checked_pixels: u32,
    pub source_coord_mismatched_pixels: u32,
    pub source_coord_out_of_bounds_pixels: u32,
    pub mean_gradient: f32,
    pub p95_gradient: f32,
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
    let source_map_distinct_sources = distinct_assigned_sources(source_map);
    let mean_gradient = if grad.is_empty() {
        0.0
    } else {
        grad.iter().sum::<f32>() / grad.len() as f32
    };
    let p95_gradient = percentile_gradient(&grad, 95.0);

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
    if mean_gradient < thresholds.min_mean_gradient {
        failures.push(format!(
            "mean_gradient {:.3} < {:.3}",
            mean_gradient, thresholds.min_mean_gradient
        ));
    }
    if p95_gradient < thresholds.min_p95_gradient {
        failures.push(format!(
            "p95_gradient {:.3} < {:.3}",
            p95_gradient, thresholds.min_p95_gradient
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
        source_map_distinct_sources,
        source_coord_checked_pixels: 0,
        source_coord_mismatched_pixels: 0,
        source_coord_out_of_bounds_pixels: 0,
        mean_gradient,
        p95_gradient,
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

fn percentile_gradient(grad: &[f32], percentile: f32) -> f32 {
    if grad.is_empty() {
        return 0.0;
    }
    let mut values = grad.to_vec();
    values.sort_by(|a, b| a.total_cmp(b));
    let index = ((values.len() - 1) as f32 * (percentile / 100.0)).round() as usize;
    values[index.min(values.len() - 1)]
}

fn distinct_assigned_sources(source_map: &SourceMap) -> usize {
    let mut seen = [false; 256];
    for &value in &source_map.data {
        if value != SourceMap::UNASSIGNED {
            seen[value as usize] = true;
        }
    }
    seen.into_iter().filter(|v| *v).count()
}

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
    let band_h = min(h, min(180, h / 3).max(80));
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
    let patch_w = min(w, min(360, w / 2).max(120));
    let patch_h = min(h, min(240, h / 3).max(100));
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
            let sx_rel0 = ox * rw / out_w;
            let sx_rel1 = ((ox + 1) * rw / out_w).max(sx_rel0 + 1).min(rw);
            let sy_rel0 = oy * rh / out_h;
            let sy_rel1 = ((oy + 1) * rh / out_h).max(sy_rel0 + 1).min(rh);
            let sx0 = x0 + sx_rel0;
            let sx1 = x0 + sx_rel1;
            let sy0 = y0 + sy_rel0;
            let sy1 = y0 + sy_rel1;
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
    fn monotonic_frame_filter_drops_leading_reversal() {
        let raw = vec![
            Position { x: 0, y: 0 },
            Position { x: -24, y: 276 },
            Position { x: -24, y: 20 },
            Position { x: -4, y: -148 },
            Position { x: -28, y: -348 },
            Position { x: -28, y: -656 },
            Position { x: 0, y: -1304 },
        ];
        let active = monotonic_active_sources(&raw);
        assert_eq!(active, vec![false, true, true, true, true, true, true]);
    }

    #[test]
    fn monotonic_frame_filter_keeps_already_monotonic_sequence() {
        let raw = vec![
            Position { x: 0, y: 0 },
            Position { x: 0, y: -150 },
            Position { x: 0, y: -300 },
        ];
        let active = monotonic_active_sources(&raw);
        assert_eq!(active, vec![true, true, true]);
    }

    #[test]
    fn motion_mask_dilation_expands_foreground_barrier() {
        let mut mask = vec![false; 25];
        mask[2 * 5 + 2] = true;
        let dilated = dilate_bool_mask(&mask, 5, 5, 1);
        let count = dilated.iter().filter(|&&v| v).count();
        assert_eq!(count, 9);
        assert!(dilated[6]);
        assert!(dilated[2 * 5 + 2]);
        assert!(dilated[3 * 5 + 3]);
        assert!(!dilated[0]);
    }

    #[test]
    fn temporal_disagreement_sees_moving_region_across_covering_frames() {
        let mut a = ImageBuffer::from_pixel(20, 20, Rgb([100, 100, 100]));
        let mut b = ImageBuffer::from_pixel(20, 20, Rgb([100, 100, 100]));
        let c = ImageBuffer::from_pixel(20, 20, Rgb([100, 100, 100]));
        a.put_pixel(10, 10, Rgb([240, 20, 20]));
        b.put_pixel(10, 10, Rgb([20, 20, 240]));
        let images = vec![a, b, c];
        let positions = vec![
            Position { x: 0, y: 0 },
            Position { x: 0, y: 0 },
            Position { x: 0, y: 0 },
        ];
        assert!(covering_temporal_disagreement(&images, &positions, 10, 10, 0) > 100.0);
        assert_eq!(
            covering_temporal_disagreement(&images, &positions, 0, 0, 0),
            0.0
        );
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
    fn local_warp_report_is_optional_for_old_reports() {
        let value = serde_json::json!({
            "shifts": [],
            "raw_positions": [{"x":0,"y":0}],
            "normalized_positions": [{"x":0,"y":0}],
            "canvas_width": 10,
            "canvas_height": 10,
            "duplicate_report": null
        });
        let report: StitchReport = serde_json::from_value(value).unwrap();
        assert!(report.local_warp.is_none());
    }

    #[test]
    fn source_coordinate_map_round_trips_and_verifies_pixels() {
        let canvas = synthetic_canvas(120, 80);
        let a = image::imageops::crop_imm(&canvas, 0, 0, 80, 80).to_image();
        let b = image::imageops::crop_imm(&canvas, 40, 0, 80, 80).to_image();
        let images = vec![a, b];
        let positions = vec![Position { x: 0, y: 0 }, Position { x: 40, y: 0 }];
        let (stitched, source_map) =
            synthesize_nearest_center_with_source_map(&images, &positions, 120, 80).unwrap();
        let coord_map = SourceCoordinateMap::from_source_map(&source_map, &positions);
        let image = coord_map.to_rgb16_image();
        let roundtrip = SourceCoordinateMap::from_rgb16_image(&image);
        let verification = verify_source_coordinate_map(&stitched, &images, &roundtrip);
        assert!(verification.checked_pixels > 0);
        assert_eq!(verification.mismatched_pixels, 0);
        assert_eq!(verification.out_of_bounds_pixels, 0);
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

    #[test]
    fn seam_motion_source_map_matches_selected_pixels() {
        let canvas = synthetic_canvas(80, 140);
        let mut a = image::imageops::crop_imm(&canvas, 0, 0, 80, 80).to_image();
        let mut b = image::imageops::crop_imm(&canvas, 0, 40, 80, 80).to_image();
        for y in 18..34 {
            for x in 25..55 {
                a.put_pixel(x, y + 40, Rgb([240, 20, 20]));
                b.put_pixel(x, y, Rgb([20, 20, 240]));
            }
        }
        let images = vec![a, b];
        let positions = vec![Position { x: 0, y: 0 }, Position { x: 0, y: 40 }];
        let (stitched, source_map) =
            synthesize_vertical_seams_motion_with_source_map(&images, &positions, 80, 120).unwrap();

        for y in 0..stitched.height() {
            for x in 0..stitched.width() {
                let source = source_map.get(x, y);
                if source == SourceMap::UNASSIGNED {
                    continue;
                }
                let source_idx = source as usize;
                let sx = x as i32 - positions[source_idx].x;
                let sy = y as i32 - positions[source_idx].y;
                assert!(sx >= 0 && sy >= 0);
                assert_eq!(
                    *stitched.get_pixel(x, y),
                    *images[source_idx].get_pixel(sx as u32, sy as u32)
                );
            }
        }
    }

    #[test]
    fn seam_motion_avoids_moving_overlap_region() {
        let base = ImageBuffer::from_pixel(90, 130, Rgb([120, 120, 120]));
        let mut a = image::imageops::crop_imm(&base, 0, 0, 90, 80).to_image();
        let mut b = image::imageops::crop_imm(&base, 0, 40, 90, 80).to_image();
        for y in 52..68 {
            for x in 30..60 {
                a.put_pixel(x, y, Rgb([255, 0, 0]));
                b.put_pixel(x, y - 40, Rgb([0, 0, 255]));
            }
        }
        let images = vec![a, b];
        let positions = vec![Position { x: 0, y: 0 }, Position { x: 0, y: 40 }];
        let (_stitched, source_map) = synthesize_vertical_seams_with_source_map_opts(
            &images,
            &positions,
            90,
            120,
            SeamOptions {
                motion_weight: 8.0,
                motion_radius: 4,
                motion_threshold: 1.0,
                motion_mask_dilate: 0,
                motion_hard_penalty: 0.0,
                foreground_mask_dilate: 0,
                foreground_mask_penalty: 0.0,
                edge_weight: 0.15,
            },
            None,
            None,
        )
        .unwrap();

        for x in 30..60 {
            let boundary_y =
                (41..80).find(|&y| source_map.get(x, y - 1) == 0 && source_map.get(x, y) == 1);
            if let Some(y) = boundary_y {
                assert!(
                    !(52..68).contains(&y),
                    "source seam crossed moving rectangle at x={x}, y={y}"
                );
            }
        }
    }

    #[test]
    fn eval_can_reject_blur_to_pass_outputs() {
        let mut sharp = RgbImage::new(180, 120);
        for y in 0..120 {
            for x in 0..180 {
                let value = if ((x / 8) + (y / 8)) % 2 == 0 { 0 } else { 255 };
                sharp.put_pixel(x, y, Rgb([value, value, value]));
            }
        }
        let blurred = ImageBuffer::from_pixel(180, 120, Rgb([128, 128, 128]));
        let mut map = SourceMap::new(180, 120);
        for y in 0..120 {
            for x in 0..180 {
                map.set(x, y, 0);
            }
        }
        let sharp_report = evaluate_stitch(&sharp, &map, EvalThresholds::default());
        let thresholds = EvalThresholds {
            min_p95_gradient: sharp_report.p95_gradient * 0.75,
            ..Default::default()
        };
        let blurred_report = evaluate_stitch(&blurred, &map, thresholds);
        assert!(!blurred_report.passed);
        assert!(
            blurred_report
                .failures
                .iter()
                .any(|failure| failure.contains("p95_gradient"))
        );
    }
}
