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

## Motion-aware seam-DP snapshot

After adding `--source-selection seam-dp-motion`, the current best visual output is:

```text
/workspace/rent-a-girlfriend/stitches/seam_motion/rent_seam_motion.png
/workspace/rent-a-girlfriend/stitches/seam_motion/rent_seam_motion_source.png
/workspace/rent-a-girlfriend/stitches/seam_motion/rent_seam_motion_report.json
/workspace/rent-a-girlfriend/stitches/seam_motion/rent_seam_motion_eval.json
/workspace/rent-a-girlfriend/stitches/seam_motion/rent_seam_motion_eval_overlay.png
/workspace/rent-a-girlfriend/stitches/seam_motion/rent_seam_motion_source_blocks.png
```

Generated with:

```bash
stitcher stitch \
  --mode vertical \
  --source-selection seam-dp-motion \
  --min-shift-y 20 \
  --max-drift-x 80 \
  --output rent_seam_motion.png \
  --report rent_seam_motion_report.json \
  --source-map rent_seam_motion_source.png \
  <timestamp-ordered frames>
```

Metrics:

```text
passed: false
image_size: 1948x2660
boundary_pixels: 26157
high_risk_boundary_pixels: 2699
largest_risky_component_area: 300
duplicate_patches: 2
source_map_distinct_sources: 7
mean_gradient: 2.0365286
p95_gradient: 8.000004
failures:
  - high_risk_boundary_pixels 2699 > 250
  - duplicate_patches 2 > 0
```

Visual assessment: this is a lot closer than the earlier blocky/nearest-center output and the foreground woman is more coherent. It still fails the hardened evaluator, which is correct: there are uneven seams left in the background and duplicate background patches remain.

A small seam-cost sweep found no real breakthrough from parameter tuning alone. Best high-risk count observed in that sweep:

```text
e0.0_mw0:
boundary_pixels: 25416
high_risk_boundary_pixels: 2566
largest_risky_component_area: 260
duplicate_patches: 2
```

This means simple local seam-cost tuning is not enough. The next improvements need better modeling of moving foreground vs static background.

## Remaining seams and artifact analysis

Top risky source handoffs in the current motion-aware output:

```text
sources [5,6]: boundary_px 3560, risk_px 853
sources [1,2]: boundary_px 2739, risk_px 547
sources [3,4]: boundary_px 3168, risk_px 470
sources [4,5]: boundary_px 2517, risk_px 465
sources [0,3]: boundary_px 2085, risk_px 299
sources [3,5]: boundary_px 620,  risk_px 225
sources [2,3]: boundary_px 2119, risk_px 220
```

Largest risky components:

```text
area 300, bbox [28, 850, 82, 218]    - long left-side/background vertical seam
area 133, bbox [1764, 672, 67, 67]   - right-side background/object seam
area 127, bbox [1739, 1504, 64, 64]  - right-side background seam patch
area 124, bbox [1920, 1156, 1, 124]  - far-right border/source transition
area 85,  bbox [1459, 1272, 43, 43]  - mid/right background seam patch
area 82,  bbox [28, 741, 1, 82]      - left-edge seam continuation
area 80,  bbox [24, 1529, 1, 80]     - left-edge seam continuation
```

Duplicate patch failures:

```text
score 0.99569124, rect_a [180, 0, 360, 240], rect_b [270, 360, 360, 240]
score 0.9931815,  rect_a [90, 0, 360, 240],  rect_b [90, 320, 360, 240]
```

Interpretation:

```text
- The foreground is closer, but source handoffs still cut through static background.
- Some seams are at canvas edges, but cropping them away would be invalid.
- Non-adjacent handoffs like [0,3] and [3,5] suggest local pairwise seam decisions are creating source-map islands.
- Duplicate patches near the top/upper-middle show background registration/source ownership is still wrong, not just visually rough.
```

## Next improvement plan

Do not use blur, crop, or fake source maps. Focus on source selection and registration.

Most promising work:

```text
1. Background-first registration
   - Build aligned-frame temporal disagreement masks.
   - Downweight/exclude the moving woman from shift estimation and seam decisions.
   - Consider local/piecewise background alignment if the pan has perspective or rolling-screen distortion.

2. Foreground-aware seam constraints
   - Dilate motion masks around skin, hair, clothing, and high-motion outlines.
   - Add a hard/high penalty for source changes through the subject.
   - Force source transitions into low-motion background corridors.

3. Globally consistent source ownership
   - Replace purely incremental pairwise paste decisions with a global/monotonic label assignment.
   - Penalize short source islands, jagged seams, and non-adjacent frame handoffs.
   - Keep source maps real by deriving the final pixels from the selected source labels.

4. Background cleanup without blur
   - Add per-source exposure/color matching before seam search if brightness shifts are driving visible transitions.
   - Allow only narrow, source-faithful feathering after the seam/source-map is already correct.
   - Broad blur remains invalid.

5. Better diagnostics
   - Emit per-source-pair heatmaps for the highest-risk handoffs.
   - Emit crops around top risky bboxes for review.
   - Add coverage/content-loss checks beyond the current crop rejection.
```

## Monotonic frame-filter experiment

A stronger source-ownership experiment was added behind:

```bash
--monotonic-frame-filter
```

This keeps the longest monotonic frame subsequence for overlap seam decisions and lets excluded reversal/outlier frames fill only pixels that would otherwise be unassigned. For this benchmark it removes the leading position reversal from overlap ownership while preserving full output size and a real source map.

Generated with:

```bash
stitcher stitch \
  --mode vertical \
  --source-selection seam-dp-motion \
  --monotonic-frame-filter \
  --min-shift-y 20 \
  --max-drift-x 80 \
  --output rent_monotonic_filter.png \
  --report rent_monotonic_filter_report.json \
  --source-map rent_monotonic_filter_source.png \
  <timestamp-ordered frames>
```

Artifacts:

```text
/workspace/rent-a-girlfriend/stitches/monotonic_filter/rent_monotonic_filter.png
/workspace/rent-a-girlfriend/stitches/monotonic_filter/rent_monotonic_filter_source.png
/workspace/rent-a-girlfriend/stitches/monotonic_filter/rent_monotonic_filter_eval.json
/workspace/rent-a-girlfriend/stitches/monotonic_filter/rent_monotonic_filter_eval_overlay.png
/workspace/rent-a-girlfriend/stitches/monotonic_filter/rent_monotonic_filter_source_blocks.png
/workspace/rent-a-girlfriend/stitches/monotonic_filter/component_crops/
```

Metrics:

```text
passed: false
image_size: 1948x2660
boundary_pixels: 23595
high_risk_boundary_pixels: 2521
largest_risky_component_area: 300
duplicate_patches: 2
source_map_distinct_sources: 7
mean_gradient: 2.0279498
p95_gradient: 8.0
failures:
  - high_risk_boundary_pixels 2521 > 250
  - duplicate_patches 2 > 0
```

Comparison to previous motion-aware seam-DP:

```text
boundary_pixels:           26157 -> 23595  (-2562)
high_risk_boundary_pixels:  2699 -> 2521   (-178)
largest_risky_component:     300 -> 300
duplicate_patches:             2 -> 2
```

Assessment: this is a real, non-cheating improvement in source ownership and reduces the source-boundary surface area, but it still does not solve the hardest background duplicates or the largest seam components. The excluded frame is not discarded from the source map entirely; it still fills unique/unassigned holes, so the output remains full-size and source-map backed.

Additional stronger prototypes were tried but not committed:

```text
background-masked alignment:
  high_risk_boundary_pixels worsened to 4254
  largest_risky_component_area worsened to 556
  duplicate_patches stayed 2

source-label island cleanup:
  slightly reduced boundary_pixels
  did not reduce duplicate_patches
  did not improve high_risk_boundary_pixels materially

simple exposure/color-bias matching:
  reduced high_risk_boundary_pixels only from 2699 to 2687
  duplicate_patches stayed 2
```

Next likely step: a true global label optimization or graph-cut style seam/source assignment with explicit foreground/motion masks. The monotonic filter helps by removing an input-order reversal from seam ownership, but remaining failures are now dominated by real background alignment/source-placement problems between adjacent retained frames.

## Best current parameterized output: monotonic frame filter + horizontal drift suppression

A follow-up sweep showed that much of the remaining left/right background seam risk came from small horizontal drift estimates in an otherwise vertical pan. Letting alignment search within a reasonable drift window but snapping all small x-shifts back to zero produced the best honest output so far:

```bash
stitcher stitch \
  --mode vertical \
  --source-selection seam-dp-motion \
  --monotonic-frame-filter \
  --min-shift-y 20 \
  --max-drift-x 40 \
  --snap-x 40 \
  --output mono.png \
  --report mono_report.json \
  --source-map mono_source.png \
  <timestamp-ordered frames>
```

Artifacts:

```text
/workspace/rent-a-girlfriend/stitches/snap_x_best/mono.png
/workspace/rent-a-girlfriend/stitches/snap_x_best/mono_source.png
/workspace/rent-a-girlfriend/stitches/snap_x_best/mono_eval.json
/workspace/rent-a-girlfriend/stitches/snap_x_best/mono_overlay.png
/workspace/rent-a-girlfriend/stitches/snap_x_best/mono_blocks.png
/workspace/rent-a-girlfriend/stitches/snap_x_best/mono_crops/
```

Metrics:

```text
passed: false
image_size: 1920x2660
boundary_pixels: 17735
high_risk_boundary_pixels: 2075
largest_risky_component_area: 125
duplicate_patches: 2
failures:
  - high_risk_boundary_pixels 2075 > 250
  - duplicate_patches 2 > 0
```

Comparison:

```text
motion-aware seam-DP:
  boundary_pixels: 26157
  high_risk_boundary_pixels: 2699
  largest_risky_component_area: 300
  duplicate_patches: 2

monotonic frame filter:
  boundary_pixels: 23595
  high_risk_boundary_pixels: 2521
  largest_risky_component_area: 300
  duplicate_patches: 2

monotonic + snap-x 40:
  boundary_pixels: 17735
  high_risk_boundary_pixels: 2075
  largest_risky_component_area: 125
  duplicate_patches: 2
```

Assessment: this is the best current output by the hardened metrics and directly targets visible uneven background seams caused by horizontal jitter. It does not crop the vertical pan; the width returns to the native frame width because the previous extra width came from small estimated x drift in a vertical pan. The duplicate patch failures remain; inspection shows the reported duplicate patches are entirely inside source frame 6 / the top source region, so they may be source-inherent repeated background rather than a duplicate introduced by stitching. Do not suppress that gate with source-map tricks; future evaluator work should distinguish source-inherent duplicates using the original inputs, not the stitched source map.

## Foreground/motion hard-mask seam experiment

A general foreground-protection seam constraint was added for `seam-dp-motion`. It does **not** hardcode this image, skin colors, hands, pockets, or crop coordinates. It builds a moving-foreground mask from temporal disagreement between all frames covering the candidate seam pixel, dilates that mask, and adds a hard seam penalty inside it.

New knobs:

```bash
--seam-motion-mask-dilate <px>
--seam-motion-hard-penalty <cost>
```

The existing soft local motion term only compares the currently pasted source with the incoming frame. The hard-mask version also checks multi-frame disagreement, which catches regions that look stable between an adjacent pair but are inconsistent across the full stack.

Best metric-oriented run from the sweep:

```bash
stitcher stitch \
  --mode vertical \
  --source-selection seam-dp-motion \
  --monotonic-frame-filter \
  --min-shift-y 20 \
  --max-drift-x 40 \
  --snap-x 40 \
  --seam-motion-radius 4 \
  --seam-motion-threshold 30 \
  --seam-motion-mask-dilate 4 \
  --seam-motion-hard-penalty 100 \
  --output t30_d4_p100.png \
  --report t30_d4_p100_report.json \
  --source-map t30_d4_p100_source.png \
  <timestamp-ordered frames>
```

Artifacts:

```text
/workspace/rent-a-girlfriend/stitches/foreground_multiframe_mask_sweep/t30_d4_p100.png
/workspace/rent-a-girlfriend/stitches/foreground_multiframe_mask_sweep/t30_d4_p100_source.png
/workspace/rent-a-girlfriend/stitches/foreground_multiframe_mask_sweep/t30_d4_p100_overlay.png
/workspace/rent-a-girlfriend/stitches/foreground_multiframe_mask_sweep/t30_d4_p100_source_blocks.png
/workspace/rent-a-girlfriend/stitches/foreground_multiframe_mask_sweep/t30_d4_p100_crops/
```

Metrics:

```text
passed: false
boundary_pixels: 17915
high_risk_boundary_pixels: 2009
largest_risky_component_area: 133
duplicate_patches: 2
failures:
  - high_risk_boundary_pixels 2009 > 250
  - duplicate_patches 2 > 0
```

Comparison to previous best (`monotonic + snap-x 40`):

```text
boundary_pixels:           17735 -> 17915  (+180)
high_risk_boundary_pixels:  2075 -> 2009   (-66)
largest_risky_component:     125 -> 133    (+8)
duplicate_patches:             2 -> 2
lower hand/pocket seam px:    199 -> 107
upper waist seam px:         3127 -> 2669
full body-middle seam px:    7299 -> 6982
```

Best hand/pocket-protection run:

```text
t80_d16_p100:
boundary_pixels: 16125
high_risk_boundary_pixels: 2042
largest_risky_component_area: 255
duplicate_patches: 2
lower hand/pocket seam px: 0
```

Assessment: the general foreground mask does what it was intended to do: it can reduce or eliminate the diagonal hand/pocket source boundary without blur/crop/fake maps. The best balanced setting (`t30_d4_p100`) slightly improves the hardened metric and cuts the hand/pocket seam roughly in half. A more aggressive setting (`t80_d16_p100`) eliminates the lower pocket seam entirely, but it moves cost elsewhere and grows the largest risky component, so it is not clearly better overall.

The remaining duplicate-patch failures are unchanged. They still appear to be in repeated top/background source content rather than the foreground hand seam. The next algorithmic step should be global label optimization or a true graph-cut style source assignment that can jointly optimize foreground protection, seam length, source order, and background duplicate avoidance.
