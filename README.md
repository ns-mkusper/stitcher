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

## Evaluation

`stitcher eval` checks whether a stitched output is cohesive and faithful instead of just visually plausible.

```bash
cargo run --release -- stitch \
  --output stitched.png \
  --report stitched.json \
  --source-map source_map.png \
  frame1.jpg frame2.jpg frame3.jpg

cargo run --release -- eval \
  --stitched stitched.png \
  --report stitched.json \
  --source-map source_map.png \
  --output eval.json \
  --overlay eval_overlay.png \
  --source-block-overlay source_blocks.png
```

The evaluator reports:

- source handoff boundary pixels
- high-risk visible seam pixels
- largest connected risky seam component
- duplicate bands/patches
- per-source handoff risk counts
- pass/fail failures

Overlay colors:

- yellow = source handoff boundary
- red = high-risk visible seam / component
- tinted regions = source frame blocks

## CI / PR tests

GitHub Actions workflow: `.github/workflows/ci.yml`

Runs:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

Tests include pan detection plus evaluator pass/fail behavior for clean outputs and visible source-block seams.
