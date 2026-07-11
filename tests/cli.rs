use assert_cmd::Command;
use image::{Rgb, RgbImage};
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
