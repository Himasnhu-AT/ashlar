# Visual regressions

`crates/ui/tests/visual_regression.rs` renders real components and independent reference
painters with the same offscreen wgpu renderer and compares the results. The reference painters
never call the components: they draw literal geometry with plain egui painter calls. Together they
define the visual contract of each control; they are not screenshots of an app.

## Coverage

| Suite | Pairs | What is compared |
|---|---|---|
| Controls | 372 | icon button, single-shape button, tab, horizontal and vertical split × light/dark × scales 1, 1.5, 2 × normal and compact sizes × the states normal, hover, press, disabled, focus, plus selected and selected-press for buttons and tabs |
| Toolbar overflow | 18 | three widths × two themes × three scales against literal rects |
| Widget palette | 12 | cells painted from `WidgetPalette::apply_to` against literal painting |
| SVG icons | 12 | catalog output rasterized by the SVG loader against literal, pre-substituted SVGs |

All 414 positive pairs must give identical RGBA pixels. The 402 pairs rendered with wgpu must
also give identical meshes: vertices (position, UV, colour), indices, texture ids and clip
rects. The SVG pairs compare the loader's rasters and must contain visible pixels.

Each state is reached with real input, and the test asserts it was reached: the control is
hovered, held down (and dragging, for split handles), or focused. Disabled controls must stay
unhovered and unfocused under the same input. Selection wins over press. Split handles are driven
in both orientations. The compact fixtures repeat every contract at a smaller size.

36 negative comparisons change one thing in the reference (fill colour, spacing, stroke width)
and must differ in both pixels and meshes, which shows the comparison can fail. There is no
tolerance, mask, crop, perceptual metric or automatic update. The toolbar fitting itself
(malformed input, reachable overflow, groups kept in order) is covered by unit tests in the
`layout` module.

## Running

```sh
cargo test -p ashlar-ui --test visual_regression
```

With `ASHLAR_UI_VISUAL_DIR=/absolute/path`, each suite writes into its own subdirectory:
`<case>-before.png` (reference), `<case>-after.png` (component), `<case>-difference.png` (changed
pixels in magenta, 255,0,128) and `comparison.tsv` with the columns `case`, `width`, `height`,
`changed_pixels`, `mesh_equal` and `expected_change`. Rows are appended as each pair finishes, so
the file is useful even when a later pair fails. Images are at most 256×144 pixels. A renderer
failure fails the test.

## Limits

The comparison is same-machine only: both sides go through the same driver in the same run, so
it says nothing about pixel identity across GPUs or drivers. It does not replace app
screenshots, complete themes, text layout checks, command dispatch, window resizing, tests
against real accessibility services, or cross-platform runtime tests. Apps that keep their own
baseline images keep their own record of where those came from.
