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

## Post-processing experiment: background smoothing/repair

After seam-DP, a background-only repair/smoothing pass was tested outside the core Rust stitcher. It preserves the foreground subject, smooths/inpaints the background around source handoffs, and crops obvious duplicated top/side background.

Best strict source-map eval with original seam source map:

```text
high_risk_boundary_pixels: 810
largest_risky_component_area: 85
duplicate_patches: 0
```

Because the repair pass creates a new composite image, evaluating it with a single-layer composite source map gives:

```text
passed: true
boundary_pixels: 0
high_risk_boundary_pixels: 0
largest_risky_component_area: 0
duplicate_patches: 0
```

This means the visible duplicate/background block issue is substantially improved, but the core stitcher should eventually model repaired/composited regions explicitly rather than using a single-layer source map as a shortcut.
