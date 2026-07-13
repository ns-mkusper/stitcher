use assert_cmd::Command;
use image::{GrayImage, Luma, Rgb, RgbImage};
use serde_json::json;
use std::path::Path;

fn synthetic_canvas(w: u32, h: u32) -> RgbImage {
    let mut img = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            img.put_pixel(
                x,
                y,
                Rgb([
                    ((x * 17 + y * 3) % 251) as u8,
                    ((x * 5 + y * 19) % 241) as u8,
                    ((x * 11 + y * 7) % 239) as u8,
                ]),
            );
        }
    }
    img
}

fn random_texture_canvas(w: u32, h: u32) -> RgbImage {
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

fn save_crop(canvas: &RgbImage, x: u32, y: u32, w: u32, h: u32, path: &Path) {
    image::imageops::crop_imm(canvas, x, y, w, h)
        .to_image()
        .save(path)
        .unwrap();
}

#[test]
fn cli_stitches_horizontal_pan_and_writes_report() {
    let tmp = tempfile::tempdir().unwrap();
    let canvas = synthetic_canvas(900, 300);
    let a = tmp.path().join("a.png");
    let b = tmp.path().join("b.png");
    let c = tmp.path().join("c.png");
    let out = tmp.path().join("out.png");
    let report = tmp.path().join("report.json");
    let source_map = tmp.path().join("source_map.png");
    let eval_report = tmp.path().join("eval.json");
    let eval_overlay = tmp.path().join("eval_overlay.png");
    save_crop(&canvas, 0, 0, 400, 300, &a);
    save_crop(&canvas, 160, 0, 400, 300, &b);
    save_crop(&canvas, 320, 0, 400, 300, &c);

    Command::cargo_bin("stitcher")
        .unwrap()
        .args([
            "stitch",
            "--output",
            out.to_str().unwrap(),
            "--report",
            report.to_str().unwrap(),
            "--source-map",
            source_map.to_str().unwrap(),
            a.to_str().unwrap(),
            b.to_str().unwrap(),
            c.to_str().unwrap(),
        ])
        .assert()
        .success();

    let img = image::open(&out).unwrap();
    assert_eq!(img.width(), 720);
    assert_eq!(img.height(), 300);
    let report_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert_eq!(report_json["canvas_width"], 720);
    assert!(report_json["shifts"][0]["dx"].as_i64().unwrap() < 0);

    Command::cargo_bin("stitcher")
        .unwrap()
        .args([
            "eval",
            "--stitched",
            out.to_str().unwrap(),
            "--report",
            report.to_str().unwrap(),
            "--source-map",
            source_map.to_str().unwrap(),
            "--output",
            eval_report.to_str().unwrap(),
            "--overlay",
            eval_overlay.to_str().unwrap(),
        ])
        .assert()
        .success();

    let eval_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(eval_report).unwrap()).unwrap();
    assert_eq!(eval_json["passed"], true);
    assert!(eval_overlay.exists());
}

#[test]
fn cli_stitches_vertical_pan_with_motion_aware_seams() {
    let tmp = tempfile::tempdir().unwrap();
    let canvas = random_texture_canvas(180, 360);
    let a = tmp.path().join("a.png");
    let b = tmp.path().join("b.png");
    let c = tmp.path().join("c.png");
    let out = tmp.path().join("out.png");
    let report = tmp.path().join("report.json");
    let source_map = tmp.path().join("source_map.png");
    let mask_dir = tmp.path().join("masks");
    std::fs::create_dir(&mask_dir).unwrap();
    save_crop(&canvas, 0, 0, 180, 160, &a);
    save_crop(&canvas, 0, 80, 180, 160, &b);
    save_crop(&canvas, 0, 160, 180, 160, &c);
    for idx in 0..3 {
        GrayImage::new(180, 160)
            .save(mask_dir.join(format!("{idx}.png")))
            .unwrap();
    }

    Command::cargo_bin("stitcher")
        .unwrap()
        .args([
            "stitch",
            "--mode",
            "vertical",
            "--source-selection",
            "seam-dp-motion",
            "--min-shift-y",
            "40",
            "--max-drift-x",
            "5",
            "--align-scale",
            "1",
            "--monotonic-frame-filter",
            "--foreground-mask-dir",
            mask_dir.to_str().unwrap(),
            "--foreground-mask-penalty",
            "100",
            "--output",
            out.to_str().unwrap(),
            "--report",
            report.to_str().unwrap(),
            "--source-map",
            source_map.to_str().unwrap(),
            a.to_str().unwrap(),
            b.to_str().unwrap(),
            c.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(out.exists());
    assert!(report.exists());
    assert!(source_map.exists());
    let report_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert!(report_json["canvas_height"].as_u64().unwrap() >= 300);
}

#[test]
fn cli_eval_rejects_fake_single_source_map_for_multi_frame_report() {
    let tmp = tempfile::tempdir().unwrap();
    let stitched = tmp.path().join("stitched.png");
    let source_map = tmp.path().join("source_map.png");
    let report = tmp.path().join("report.json");
    let eval_report = tmp.path().join("eval.json");

    synthetic_canvas(120, 80).save(&stitched).unwrap();
    let mut map = GrayImage::new(120, 80);
    for pixel in map.pixels_mut() {
        *pixel = Luma([1]);
    }
    map.save(&source_map).unwrap();
    std::fs::write(
        &report,
        serde_json::to_vec_pretty(&json!({
            "shifts": [{"dx": -40, "dy": 0, "score": 1.0}],
            "raw_positions": [{"x":0,"y":0},{"x":40,"y":0}],
            "normalized_positions": [{"x":0,"y":0},{"x":40,"y":0}],
            "canvas_width": 120,
            "canvas_height": 80,
            "duplicate_report": null
        }))
        .unwrap(),
    )
    .unwrap();

    Command::cargo_bin("stitcher")
        .unwrap()
        .args([
            "eval",
            "--stitched",
            stitched.to_str().unwrap(),
            "--report",
            report.to_str().unwrap(),
            "--source-map",
            source_map.to_str().unwrap(),
            "--output",
            eval_report.to_str().unwrap(),
        ])
        .assert()
        .failure();

    let eval_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(eval_report).unwrap()).unwrap();
    assert!(
        eval_json["failures"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f.as_str().unwrap().contains("source_map_distinct_sources"))
    );
}

#[test]
fn cli_eval_rejects_unapproved_crop() {
    let tmp = tempfile::tempdir().unwrap();
    let stitched = tmp.path().join("stitched.png");
    let source_map = tmp.path().join("source_map.png");
    let report = tmp.path().join("report.json");
    let eval_report = tmp.path().join("eval.json");

    synthetic_canvas(100, 70).save(&stitched).unwrap();
    let mut map = GrayImage::new(100, 70);
    for pixel in map.pixels_mut() {
        *pixel = Luma([1]);
    }
    map.save(&source_map).unwrap();
    std::fs::write(
        &report,
        serde_json::to_vec_pretty(&json!({
            "shifts": [],
            "raw_positions": [{"x":0,"y":0}],
            "normalized_positions": [{"x":0,"y":0}],
            "canvas_width": 120,
            "canvas_height": 80,
            "duplicate_report": null
        }))
        .unwrap(),
    )
    .unwrap();

    Command::cargo_bin("stitcher")
        .unwrap()
        .args([
            "eval",
            "--stitched",
            stitched.to_str().unwrap(),
            "--report",
            report.to_str().unwrap(),
            "--source-map",
            source_map.to_str().unwrap(),
            "--output",
            eval_report.to_str().unwrap(),
        ])
        .assert()
        .failure();

    let eval_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(eval_report).unwrap()).unwrap();
    assert!(
        eval_json["failures"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f.as_str().unwrap().contains("cropped_output"))
    );
}

#[test]
fn cli_eval_writes_high_risk_component_crops() {
    let tmp = tempfile::tempdir().unwrap();
    let stitched = tmp.path().join("stitched.png");
    let source_map = tmp.path().join("source_map.png");
    let report = tmp.path().join("report.json");
    let eval_report = tmp.path().join("eval.json");
    let crops_dir = tmp.path().join("component_crops");

    let mut img = RgbImage::new(120, 80);
    let mut map = GrayImage::new(120, 80);
    for y in 0..80 {
        for x in 0..120 {
            if x < 60 {
                img.put_pixel(x, y, Rgb([0, 0, 0]));
                map.put_pixel(x, y, Luma([1]));
            } else {
                img.put_pixel(x, y, Rgb([255, 255, 255]));
                map.put_pixel(x, y, Luma([2]));
            }
        }
    }
    img.save(&stitched).unwrap();
    map.save(&source_map).unwrap();
    std::fs::write(
        &report,
        serde_json::to_vec_pretty(&json!({
            "shifts": [{"dx": 0, "dy": -40, "score": 1.0}],
            "raw_positions": [{"x":0,"y":0},{"x":0,"y":40}],
            "normalized_positions": [{"x":0,"y":0},{"x":0,"y":40}],
            "canvas_width": 120,
            "canvas_height": 80,
            "duplicate_report": null
        }))
        .unwrap(),
    )
    .unwrap();

    Command::cargo_bin("stitcher")
        .unwrap()
        .args([
            "eval",
            "--stitched",
            stitched.to_str().unwrap(),
            "--report",
            report.to_str().unwrap(),
            "--source-map",
            source_map.to_str().unwrap(),
            "--output",
            eval_report.to_str().unwrap(),
            "--max-high-risk-boundary-pixels",
            "0",
            "--component-crops-dir",
            crops_dir.to_str().unwrap(),
        ])
        .assert()
        .failure();

    let crops: Vec<_> = std::fs::read_dir(&crops_dir).unwrap().collect();
    assert!(crops.len() >= 2, "expected stitched/source overlay crops");
}
