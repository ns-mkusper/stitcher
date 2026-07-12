# Rent-a-Girlfriend moving foreground benchmark

This benchmark tracks the difficult moving-foreground pan from `Photos-2-001.zip`.

## Current best output

Generated with:

```bash
stitcher stitch \
  --mode vertical \
  --source-selection seam-dp \
  --min-shift-y 20 \
  --max-drift-x 80 \
  --output rent_seam_timestamp.png \
  --report rent_seam_timestamp_report.json \
  --source-map rent_seam_timestamp_source.png \
  <timestamp-ordered frames>

stitcher eval \
  --stitched rent_seam_timestamp.png \
  --report rent_seam_timestamp_report.json \
  --source-map rent_seam_timestamp_source.png \
  --output rent_seam_timestamp_eval.json \
  --overlay rent_seam_timestamp_eval_overlay.png \
  --source-block-overlay rent_seam_timestamp_source_blocks.png
```

## Baseline metrics

Previous nearest-center/block output:

```text
boundary_pixels: 27408
high_risk_boundary_pixels: 4907
largest_risky_component_area: 326
```

Current seam-DP output:

```text
passed: false
boundary_pixels: 25260
high_risk_boundary_pixels: 2623
largest_risky_component_area: 260
duplicate_patches: 2
failures:
  - high_risk_boundary_pixels 2623 > 250
  - duplicate_patches 2 > 0
```

## Assessment

This is a major improvement over rectangular/nearest-center source handoffs. The foreground female is close to coherent, but the background still has bad seams/duplicated patches. Next work should focus on background consistency and motion-aware seam penalties rather than broad blending.

## Invalidated post-processing experiment: background smoothing/repair

A background smoothing/repair pass was tested after seam-DP, but it is **not** considered a valid improvement path.

Why it is invalid:

```text
- It used blur to hide artifacts instead of fixing registration/source selection.
- It cropped away real content, including part of the subject in the taller pan.
- It could be made to pass seam metrics with a fake single-layer source map.
- It still left duplicate background content in the image.
```

The only honest use for background blur is a small final touch-up after the stitch is already high confidence. It must not be used to reach the next quality level or to make evaluator metrics pass.

Future evaluator work must add explicit regression gates for:

```text
- no content-loss/cropping unless explicitly requested
- no sharpness/edge-energy collapse from blur
- no fake source-map shortcuts for multi-frame outputs
- duplicate-patch detection independent of source map
```

The current honest benchmark remains the seam-DP output above, which still fails and should be improved by better background registration, motion-aware seam costs, and foreground-aware source selection.
