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
