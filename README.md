# stitcher

Rust CLI + web GUI for reconstructing a single still from ordered screenshots of a camera pan.

## Features

- auto-detects simple vertical or horizontal pan direction
- supports up/down/left/right pan motion
- uses opposite-signed canvas placement to avoid stacked duplicate overlap blocks
- synthesizes the final canvas from the source viewport closest to each pixel
- optional duplicate-region success gate
- browser GUI for uploading images and downloading the stitched result
- CI/PR tests with fmt, clippy, and automated stitching tests

## CLI

```bash
cargo run --release -- stitch \
  --output stitched.png \
  --check-duplicates \
  frame1.jpg frame2.jpg frame3.jpg
```

Modes:

```bash
--mode auto        # default
--mode vertical
--mode horizontal
```

Useful tuning flags:

```bash
--min-shift-y 30
--max-shift-y <pixels>
--min-shift-x 30
--max-shift-x <pixels>
--max-drift-x 60
--max-drift-y 60
--align-scale 4
```

## Web GUI

```bash
cargo run --release -- serve --bind 127.0.0.1:3000
```

Open `http://127.0.0.1:3000`, upload ordered screenshots, and download the result.

## CI / PR tests

GitHub Actions workflow: `.github/workflows/ci.yml`

Runs:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```
