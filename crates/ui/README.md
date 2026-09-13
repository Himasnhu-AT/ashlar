# ashlar-ui

General-purpose egui components for ashlar apps: design tokens, inputs with undo-friendly edit
phases, virtualized lists, dialogs and toasts, the command UI (menu bar, palette, keymap editor)
and automation ids so every control can be inspected and clicked by an agent. Domain widgets
(timelines, canvases, meters) stay in the apps and are built from these pieces.

Run the gallery to see every component in its states, light and dark:

```sh
cargo run -p ashlar-example-gallery
cargo run -p ashlar-example-gallery -- --snapshot /tmp/gallery   # PNGs, no window
```

## Components

| Module | What it gives you |
|---|---|
| `tokens` | Design tokens (surfaces, text, accent, selection, focus, borders, status colours, radii, spacing and type scales), `apply`/`install`, light/dark/follow-system, interface scale, app token extensions |
| `fonts` | `FontInstall`: faces from app-supplied bytes (validated), primary/named families, fallbacks, CJK chain ordered by UI language |
| `edit` | `Edit { phase: Begin/Update/Commit/Cancel, start, value }`: one drag or typed entry = one undo step |
| `value_field` | Numeric drag/scrub field (Shift ×10, Alt ×0.1, click or Enter to type, arrows step, Escape restores); `ValueFormat` for formatted fields (timecode, units) |
| `slider` | `Slider` (track follows the pointer, click jumps) and `Knob` (vertical drag, double-click resets) |
| `swatches` | `SwatchGrid` with selection, checkerboard for transparent colours, arrow-key focus |
| `search` | `SearchField`: glyph, clear button, Escape clears then unfocuses, length cap |
| `virtual_list` | `VirtualList` and `VirtualTable` (resizable columns, sort indicator): uniform or variable row heights, keyboard selection, O(visible) per frame |
| `tree` | `TreeView` over a `TreeModel`, serializable `TreeState`, flattening cached per revision, keyboard expand/collapse |
| `dialog` | Modal dialog shell: Enter → default button (unless a multi-line editor has focus), Escape → cancel |
| `toasts` | Bounded toast queue, timed or sticky, hover pauses, one scheduled repaint, zero cost when empty |
| `status_bar` | Bottom bar with left/right sections: text, buttons, determinate/indeterminate progress |
| `commands` | `menu_bar` drawing `ashlar_command::menu::MenuSpec` (re-exported; from `CommandInfo` menu paths; serializable; the same spec feeds the native menu bar), `CommandPalette` (fuzzy, shortcuts, disabled reasons), `KeymapEditor` (record, conflicts, reset/clear), `dispatch` (keys → command id) |
| `automation` | Per-pass registry of id, label, role, rect, value, selection; `inspect()` for `ui.inspect`, `find`/`click_point` for `ui.click` |
| `cache` | `RevisionCache`, `GalleyCache`, `MeshCache`, and a context-wide `cache::galley` |
| `buttons`, `tabs`, `layout`, `docking`, `theme`, `icons`, `printing` | The shared components seeded from an earlier UI library: icon buttons, tab fitting, splits and resize handles, toolbar overflow, panel docking, widget-state colouring, SVG catalogs and print settings (see [Shared components](#shared-components)) |

## Design tokens

```rust
use ashlar_ui::tokens::{self, ThemeMode, Tokens};

fn setup(ctx: &egui::Context) {
    let dark = Tokens::dark().with_accent(egui::Color32::from_rgb(232, 89, 12));
    tokens::install(ctx, &Tokens::light(), &dark, ThemeMode::System);
    tokens::set_interface_scale(ctx, 1.25);
}

fn paint(ui: &egui::Ui) {
    let t = tokens::current(ui.ctx()); // the active theme's set
    ui.painter().rect_filled(ui.max_rect(), t.radius.md, t.surface.sunken);
}
```

`apply` also writes egui's own visuals, spacing and text styles, so stock egui widgets match.
Apps add their own domain colours with `tokens::set_extension(ctx, theme, MyTokens { .. })` and
read them with `tokens::extension::<MyTokens>(ctx)`.

## Fonts

ashlar ships no font files. The app supplies bytes; a corrupt face is an error at `add`, not a
panic inside egui later:

```rust
use ashlar_ui::fonts::{CjkScript, FontInstall, FontError};

fn fonts(ctx: &egui::Context, ui: &'static [u8], mono: &'static [u8], jp: Vec<u8>, sc: Vec<u8>) -> Result<(), FontError> {
    let mut f = FontInstall::new();
    f.add("ui", ui)?.add("mono", mono)?;
    f.primary(egui::FontFamily::Proportional, "ui")?.primary(egui::FontFamily::Monospace, "mono")?;
    f.add_cjk(CjkScript::Japanese, "jp", jp)?.add_cjk(CjkScript::SimplifiedChinese, "sc", sc)?;
    if let Some(s) = CjkScript::for_language("ja-JP") {
        f.prefer(s);
    }
    f.install(ctx);
    Ok(())
}
```

## Edit phases and undo

Value components return `Option<Edit>`. Map it onto history merge keys: open a group on
`Begin`, apply `Update`s with the group's merge key, close it on `Commit`, roll back on `Cancel`.
A `Commit` with no `Begin` before it (arrow key, slider click, typed value) is a one-shot change.

```rust
use ashlar_ui::edit::EditPhase;
use ashlar_ui::value_field::{Decimal, ValueField};

fn opacity(ui: &mut egui::Ui, value: &mut f64) {
    let fmt = Decimal { decimals: 0, suffix: " %" };
    if let Some(e) = ValueField::new("inspector.opacity", "Opacity", value, &fmt).range(0.0..=100.0).show(ui).edit {
        match e.phase {
            EditPhase::Begin => { /* history.begin_group() */ }
            EditPhase::Update | EditPhase::Commit => { /* apply e.value (merge key = field id) */ }
            EditPhase::Cancel => { /* history.cancel_group() */ }
        }
    }
}
```

## Virtualized lists

Only visible rows are laid out, painted and registered. Uniform heights cost nothing per row;
variable heights are summed once into an offset table per `revision` (O(rows)), then each frame
binary-searches the first visible row. The tests drive 100 000 rows and assert that a frame
paints only the visible rows, that the height function is not called again for an unchanged
revision, and that frame time at 100k rows matches 1k rows (≈0.3 ms per frame in a debug build).

## Commands

```rust
use ashlar_command::{CommandInfo, Keymap, shortcut::Platform};
use ashlar_ui::commands::{dispatch, menu_bar, CommandPalette, MenuSpec};

fn frame(ui: &mut egui::Ui, infos: &[CommandInfo], keymap: &Keymap, palette: &mut CommandPalette, revision: u64) -> Option<String> {
    let mut run = dispatch(ui.ctx(), keymap);                       // keyboard
    let spec = MenuSpec::from_commands(infos, &["File", "Edit"]);   // also feeds a native menu
    run = run.or(menu_bar(ui, &spec, Platform::current()));        // in-window menu bar
    run.or(palette.show(ui.ctx(), infos, revision, Platform::current()))
}
```

Every returned id goes through the app's one `execute` chokepoint.

## Automation

Each component registers `id → (label, role, rect, value, selected)` for the visible part of
itself, every pass, without allocating once warm. Ids are the dotted strings the app passes
(`inspector.opacity`); components add sub-ids (`files.row.12`, `dialog.button.0`,
`menu.item.file.save`, `toast.3.close`). Call `automation::install(ctx)` at startup, then
`automation::inspect(ctx)` serves `ui.inspect` and `automation::click_point(ctx, id)` serves
`ui.click`.

## Shared components

These modules are the shared building blocks seeded into ashlar from an earlier UI library. They
hold no application state: the app passes its data in and reads back what the user asked for.

### SVG catalogs

`icons::SvgIconSet` prepares a fixed set of embedded monochrome SVGs once and hands egui the same
shared bytes every time it is asked.

```rust
use std::sync::OnceLock;
use ashlar_ui::icons::{SvgIconSet, SvgStyle};

static ICONS: &[(&str, &[u8])] = &[("save", include_bytes!("icons/save.svg"))];

fn icons() -> &'static SvgIconSet {
    static SET: OnceLock<SvgIconSet> = OnceLock::new();
    SET.get_or_init(|| {
        let style = SvgStyle { current_color: "#0f172a", stroke_width: Some("1.75") };
        SvgIconSet::new(ICONS, style, "bytes://my-app/icons/light/", "save")
    })
}

fn toolbar(ui: &mut egui::Ui) {
    ui.add(egui::Image::new(icons().source("save")).fit_to_exact_size(egui::vec2(16.0, 16.0)));
}
```

The app installs the SVG loader (`egui_extras::install_image_loaders` with its `svg` feature) and
chooses tint and size; the library installs no loader and no theme. A catalog is immutable after
`new` and every lookup returns the same `Arc` bytes. An unknown name gets the fallback icon's
bytes but keeps the requested name in its URI; with no fallback either, the bytes are empty and the
loader reports the error. `contains` and `canonical_name` only see real entries, so resolve
unknown names to the fallback yourself first if you want a single cache entry per icon. Give
each catalog and style its own URI prefix within a context. Lookups never add entries. Only use
it for trusted, compiled-in monochrome assets: the only processing is decoding as lossy UTF-8 and
two literal substitutions (`currentColor`, and `stroke-width="2"` when a width is given).

### Icon buttons

`buttons::IconButton` takes an accessible label, a size and an `IconButtonStyle`: one
`ButtonVisuals` (fill, stroke, icon colour) for each of normal, hovered, pressed and selected,
plus a corner radius and a focus stroke. Selected wins over everything; `pressed: None` keeps the
hovered look while the button is held, `Some` gives it its own. Press capture and cancelling a
press by moving away are egui's.

```rust
use ashlar_ui::buttons::{ButtonVisuals, IconButton, IconButtonStyle};
use egui::{Color32, CornerRadius, Stroke, vec2};

let base = ButtonVisuals { fill: Color32::TRANSPARENT, stroke: Stroke::NONE, icon: theme.text };
let style = IconButtonStyle {
    normal: base,
    hovered: ButtonVisuals { fill: theme.hover, ..base },
    pressed: Some(ButtonVisuals { fill: theme.pressed, ..base }),
    selected: ButtonVisuals { fill: theme.accent, icon: Color32::WHITE, ..base },
    corner_radius: CornerRadius::same(4),
    focus_stroke: Stroke::new(1.0, theme.focus),
};
let clicked = ui
    .push_id("brush-tool", |ui| {
        IconButton::new(&t("Brush"), vec2(28.0, 28.0), &style)
            .selected(tool == Tool::Brush)
            .show(ui, |ui, rect, color| paint_brush(ui.painter(), rect, color))
    })
    .inner
    .clicked();
```

Take the colours from your theme and the label from your translations. A disabled parent UI
disables and fades the button. The label and selected state go to AccessKit, keyboard focus draws
an outline inset by two points, and Enter, Space and the mouse activate it through egui. The
button never runs a command or toggles itself: act on `clicked()` and pass `selected` back next
frame. Tooltips go on the returned response. `single_shape_frame` paints fill and border as one
`painter.rect` call, for controls that used to be drawn that way. The id comes from the UI
position, so wrap buttons in `push_id` when they can be reordered. Sizes must be finite, positive
and at most 4096 points (otherwise the button is inert), and strokes are limited to half the
button's smaller side.

### Tabs

`tabs::fit(widths, selected, available, minimum, overflow_width)` decides which tabs of a strip are
shown and how wide. Tabs shrink before any is hidden, the widest first, down to the minimum. When
that is not enough, an overflow button is reserved, the selected tab always stays, and other tabs
keep their order. A tab can end up zero wide in a very narrow strip. Every input is sanitized, so
the result is finite and never wider than `available`; it runs in linear time with at most 32
bisection passes.

```rust
use ashlar_ui::tabs::Tab;

let response = Tab::new(ui.id().with(("doc-tab", doc.id)), &doc.title, doc.id == current)
    .sense(egui::Sense::click_and_drag())
    .show_at(ui, rect, |ui, r| paint_tab(ui.painter(), rect, r.hovered(), doc.id == current));
if response.clicked() {
    current = doc.id;
}
```

The caller owns ids, the selection, labels, the overflow menu, painting and drags. `Tab` reports
the AccessKit Tab role with its selected state, activates on Enter and Space, and draws the
theme's focus outline after your painter. Close buttons and context menus are separate widgets.

### Splits and resize handles

`layout::split_rect(rect, axis, size, gap, minimum)` divides a rect along `SplitAxis::Horizontal`
(first pane left) or `Vertical` (first pane on top). `SplitSize` is a ratio of the space, a fixed
first size or a fixed second size. The result has `first`, `divider` and `second` rects. The gap
and minimum come from the app; in a space too small for two minimums each pane gets half.
Non-finite or inverted input gives `None`.

```rust
use ashlar_ui::layout::{ResizeHandle, ResizeHandleStyle, SplitAxis, split_rect};

if let Some(split) = split_rect(rect, SplitAxis::Horizontal, prefs.sidebar, 4.0, 120.0) {
    sidebar(ui, split.first);
    editor(ui, split.second);
    let style = ResizeHandleStyle { hovered: theme.hover, dragged: theme.accent, focused: theme.focus };
    let moved = ResizeHandle::new(ui.id().with("sidebar"), SplitAxis::Horizontal, split.divider, &t("Resize sidebar"), style)
        .value(split.first_size(), 120.0..=split.available() - 120.0)
        .show(ui);
    if let Some(size) = split.resized(prefs.sidebar, moved.delta, 120.0) {
        prefs.sidebar = size;
    }
}
```

`ResizeHandle` reports one delta per frame from the pointer, from the arrow keys of its axis while
focused, and from AccessKit increment and decrement. `value` adds the current position and its
bounds to the accessibility node and enables set-value. Tab still moves focus away. Invalid
geometry gives an inert handle. Apply the delta with `SplitLayout::resized`; nothing is stored by
the widget.

### Panel docking

`docking::Layout<P>` arranges panels in tab groups, splits, accordions and floating groups,
independently of what the panels contain. Changes are `Action`s applied as validated, atomic
transactions; `location` and `restore` hide a panel and put it back where it was. With the
`serde` feature the model and actions serialize. `DockArea` is an optional renderer that only
reports actions; protected panels such as a document canvas render without a tab header, and
per-panel size limits shape the view without overwriting saved sizes. Apps with their own chrome
use `resolve_drop` for the same drop rules. See [the docking guide](../../docs/ui/ui-docking.md).

### Toolbar overflow

`layout::fit_toolbar(widths, available, spacing, overflow_width)` keeps a leading run of toolbar
groups at their natural widths and moves the rest to an overflow control. It reserves the whole
overflow width plus the gap before it, and never compresses a group or skips one to fit a later
one. The app paints the overflow control as an enabled, focusable button whose menu offers every
overflowed group. It returns `None` for malformed input and when not even the overflow control
fits; the app needs its own fallback for that case (a menu button only, say).

### Theme mechanisms

`theme::WidgetPalette` sets the background fills (both `bg_fill` and `weak_bg_fill`), corner radius
and foreground stroke of egui's five widget states, and leaves borders and expansion alone.

```rust
use ashlar_ui::theme::WidgetPalette;

let mut style = (*ctx.style()).clone();
WidgetPalette { noninteractive: c.surface, inactive: c.control, hovered: c.hover, active: c.pressed, open: c.open }
    .apply_to(&mut style.visuals.widgets, egui::CornerRadius::same(4), egui::Stroke::new(1.0, c.text));
style.visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, c.accent); // an app-specific exception afterwards
ctx.set_style(style);
```

It is the low-level piece that `tokens` uses when it installs a theme.

### Print settings and feedback

`printing::State` holds the transient part of a print dialog: the chosen printer, copies, duplex,
grayscale, and the receivers of the discovery and submission workers. The host supplies those
workers (`Discover`, and a `Submission` receiver for each job) and maps `Settings` to the print
crate's `Job`. `show` draws the controls and needs a stable id and localized `Labels`.
Progress and results come back as typed `Feedback`, and `poll` schedules its own repaints while
work is pending.

A refresh blocks submission until the list arrives. A selection that is still in the new list is
kept; otherwise the default printer is chosen, or the first. An empty discovery clears the
selection and a failed one empties the list, so a printer that no longer exists can never look
ready, and `ready` is false while anything is pending. `begin` refuses to replace a running job, and dropping a receiver does not
cancel the job behind it. `duplex: None` means the queue's default. Receipts and error texts are
passed through untouched. The controls work on WASM too.

## Compatibility and checks

egui 0.36, Rust 1.95+. The production dependency disables egui's default features: fonts and
rendering stay the application's choice.

```sh
cargo clippy -p ashlar-ui --all-targets -- -D warnings
cargo test -p ashlar-ui
cargo test -p ashlar-example-gallery
cargo check --target wasm32-unknown-unknown -p ashlar-ui
```

[Visual regressions](../../docs/ui/ui-visual-regressions.md) describes the paired comparisons of
the shared controls against reference painters. To keep the rendered images and the comparison
table:

```sh
ASHLAR_UI_VISUAL_DIR=/absolute/path/to/visuals cargo test -p ashlar-ui --test visual_regression
```
