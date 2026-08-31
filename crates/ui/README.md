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
| `buttons`, `tabs`, `layout`, `docking`, `theme`, `icons`, `printing` | Seeded from the earlier shared UI library; see below |

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

## Seeded components

### SVG catalogs

```rust
use std::sync::OnceLock;
use ashlar_ui::icons::{SvgIconSet, SvgStyle};

// Supply the application's own attributed assets here.
static ICONS: &[(&str, &[u8])] = &[];

fn icons() -> &'static SvgIconSet {
    static ICONS_SET: OnceLock<SvgIconSet> = OnceLock::new();
    ICONS_SET.get_or_init(|| SvgIconSet::new(
        ICONS,
        SvgStyle { current_color: "#ffffff", stroke_width: Some("1.75") },
        "bytes://my-app/icons/",
        "square",
    ))
}

fn icon(name: &str) -> egui::Image<'static> {
    egui::Image::new(icons().source(name))
}
```

The application installs `egui_extras::install_image_loaders` with its `svg` feature before drawing.
It sets tint and size on the returned image.
The library does not install loaders or an egui theme.

The catalog is immutable. SVGs are prepared once and held in `Arc<[u8]>`; lookups share those bytes.
The fallback uses the requested name in its URI, matching earlier catalogs. Missing both
the requested name and fallback produces empty bytes for the loader to report as an error, without
indexed lookups or a panic. `contains` and `canonical_name` inspect only actual catalog entries. An app can resolve an unknown
name to its fallback before calling `source` to keep a single cache key.

Use a different URI prefix for every catalog/style combination used in the same egui context.
egui caches by URI; supplying different bytes under one URI would reuse stale content. Apps with
their own contexts can retain their current prefixes. Unknown names do not add entries to this
catalog, though the egui loader may cache a requested URI.

This is an adapter for trusted, compiled-in monochrome assets, not an SVG sanitizer. It performs
literal `currentColor` and optional `stroke-width="2"` substitutions, keeps other widths and explicit
colors untouched, and uses the existing lossy UTF-8 behavior. General SVG parsing and rasterization
remain the responsibility of the existing loader.

### Icon buttons

`buttons::IconButton` accepts a label, size and `IconButtonStyle`, and returns `egui::Response`.
`ButtonVisuals` describes fill, border and icon tint for normal, hovered, pressed and selected
states. Selection wins over press and hover. `pressed: None` keeps ordinary hover/normal styling;
`Some` supplies a separate press treatment. egui owns pointer capture and click cancellation.

```rust
use ashlar_ui::buttons::{ButtonVisuals, IconButton, IconButtonStyle};
use egui::{Color32, CornerRadius, Stroke, Vec2};

fn layer_button(ui: &mut egui::Ui, selected: bool) -> egui::Response {
    let normal = ButtonVisuals {
        fill: Color32::TRANSPARENT, stroke: Stroke::NONE, icon: Color32::WHITE,
    };
    let style = IconButtonStyle {
        normal,
        hovered: ButtonVisuals { fill: Color32::DARK_GRAY, ..normal },
        pressed: None,
        selected: ButtonVisuals { fill: Color32::BLUE, ..normal },
        corner_radius: CornerRadius::same(4),
        focus_stroke: Stroke::new(1.0, Color32::LIGHT_BLUE),
    };
    IconButton::new("Layers", Vec2::splat(28.0), &style).selected(selected)
        .show(ui, |ui, rect, tint| {
            // Use the application's SVG catalog or its existing painter icon here.
            ui.painter().circle_filled(rect.center(), 5.0, tint);
        })
}
```

Use actual theme colors in the style adapter and localized text for the label. The painter receives
the full button rectangle and icon tint; the app chooses icon padding/size. Parent disabled state
applies to painting, interaction and accessibility. The library reports label and selection to
AccessKit and draws the supplied inset focus outline. Mouse, Enter and Space activation come from
egui. The caller reads `clicked()`, runs its command and updates selection; the component neither
dispatches nor toggles state. Attach tooltips to the returned response.

Controls previously painted with one `egui::Painter::rect` can opt into `.single_shape_frame()`.
This keeps their fill/border edge tessellation, especially for translucent disabled borders.
The default paints fill and border separately, preserving existing consumers' rendering.

IDs come from the surrounding UI scope/position, independent of the translated label. Use
`ui.push_id(stable_id, ...)` when items reorder. Dimensions must be finite, positive and no larger
than 4096 points; other sizes produce an inert zero-size response. Invalid stroke widths are
ignored and large strokes are bounded to half the smaller button dimension.

### Tabs

`tabs::fit(natural_widths, selected, available, minimum, overflow_width)` returns visible tab
indices and widths, overflow indices, and the actual reserved overflow width. It shrinks wide
tabs before hiding them, keeps input order and retains the selected tab. Tiny strips may reduce
its width to zero. Negative/non-finite dimensions become zero and large dimensions are bounded.
The work is linear in tab count with at most 32 fitting passes; the caller caps its own tab count.

`tabs::Tab` registers a tab at a supplied rectangle and invokes the application's painter:

```rust
fn tab(ui: &mut egui::Ui, rect: egui::Rect, selected: bool) -> egui::Response {
    ashlar_ui::tabs::Tab::new(ui.id().with("layers-tab"), "Layers", selected)
        .sense(egui::Sense::click_and_drag())
        .show_at(ui, rect, |ui, response| {
            let fill = if response.hovered() { egui::Color32::GRAY } else { egui::Color32::DARK_GRAY };
            ui.painter().rect_filled(response.rect, 0.0, fill);
        })
}
```

Supply a stable ID independent of the translated label. The component reports the accessible
Tab role, label and selection, handles egui keyboard activation, and paints a focus outline from
the caller's theme. The caller keeps selection, labels, overflow menus, painting and drag actions;
close buttons and tab context menus stay separate. Dock and floating-panel adapters use the same
component without sharing window or workspace state.

### Splits and resize handles

`layout::split_rect` divides a rectangle along `SplitAxis::Horizontal` (left/right children) or
`Vertical` (top/bottom). `SplitSize` accepts a ratio, fixed first pane, or fixed second pane in
logical points. The result contains `first`, `divider` and `second` rectangles. Gap and minimum
are supplied by the application. Tiny areas relax the minimum to half the space available;
non-finite or inverted input returns `None` instead of panicking.

```rust
use ashlar_ui::layout::{split_rect, SplitAxis, SplitSize};

fn two_panes(rect: egui::Rect) -> Option<(egui::Rect, egui::Rect)> {
    let split = split_rect(rect, SplitAxis::Horizontal, SplitSize::Ratio(0.5), 4.0, 20.0)?;
    Some((split.first, split.second))
}
```

`layout::ResizeHandle` receives a stable ID, axis, divider rectangle, localized label and
`ResizeHandleStyle` colors. `show` returns an egui response and a movement delta for that frame.
Pointer drags, focused axis-arrow keys and AccessKit increment/decrement actions all produce
that delta. `value(current, range)` adds accessible numeric bounds and SetValue support. Tab
continues normal focus traversal. Invalid geometry or numeric parameters make the handle inert.

Apply the delta through `SplitLayout::resized(size, delta, minimum)` to retain fixed/ratio sizing,
or use an application-specific adapter. The library does not persist geometry, dispatch commands
or create panes. Apps keep their existing dock trees, stacked panels, layout files, pane contents,
cameras, document focus and native windows. Shared controls do not constitute a dock-tree migration.

### Panel docking

`docking::Layout<P>` owns panel placement independently of application content: a tree of split,
tab and accordion nodes plus floating tab groups. `apply(Action)` validates the complete layout,
applies a proposed change to a copy and commits only a valid result. Moves, tab insertion/reorder,
side splits, detach, activate, close, reveal and resizing use this one transaction path. Panel IDs
must be unique, stable values; labels and document state are supplied by the app.

`Layout::location` captures a panel's placement before hiding it; `restore` returns it there if
the remembered destination still exists. Apps choose an explicit fallback when it does not.
Enable the optional `serde` feature to persist the shared layout, or convert to/from an existing
workspace schema. Validate loaded input before rendering or recursively converting it.

`DockArea` is an optional renderer. It takes a layout snapshot, styles, labels, panel permissions
and body callbacks, then returns actions for the app's command handler. Its floating groups stay
inside the current egui viewport. Protected document canvases have no tab header. Per-panel size
limits constrain geometry without overwriting saved preferred widths when the window shrinks.
Applications with established chrome can use the shared transaction model and `resolve_drop`
without changing their renderer. See [the docking guide](../../docs/ui/ui-docking.md) for details.

### Toolbar overflow

`layout::fit_toolbar(natural_widths, available, spacing, overflow_width)` keeps an ordered
prefix of indivisible groups at their natural widths. When groups do not fit, it reserves the
full overflow-control width and the preceding gap. `ToolbarFit` contains visible `(index, width)`
pairs, overflow indices and the reserved width. It does not skip wide groups or compress controls.

The application paints an enabled, focusable overflow control and makes every overflow index
available in its menu through existing commands. Group identity, labels, selection and command
dispatch remain local. An empty input produces an empty fit. Invalid dimensions, more than 4096
groups, or an available width too small for the overflow control return `None`; the caller must
then provide a scrollable row, wrapping or an existing application menu. Never turn that result
into silently missing commands. See the API documentation for the accepted numeric bounds.

### Theme mechanisms

`theme::WidgetPalette` applies background colors to egui's five widget states, with a supplied
corner radius and foreground stroke. Apps with their own token schema construct it from those tokens;
[`tokens`](#design-tokens) uses it internally. It updates each state's strong/weak background fill, radius and foreground; borders,
expansion and all other visual settings are retained.

```rust
use ashlar_ui::theme::WidgetPalette;
use egui::{Color32, CornerRadius, Stroke};

fn style_widgets(visuals: &mut egui::Visuals, palette: WidgetPalette, text: Color32) {
    palette.apply_to(&mut visuals.widgets, CornerRadius::same(4), Stroke::new(1.0, text));
    // Optional app-specific exceptions belong after the shared operation.
    visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
}
```

This is the low-level mechanism; [`tokens`](#design-tokens) is the complete theme. See
[theme infrastructure](../../docs/ui/theme-infrastructure.md) for the comparison it
came from.

### Print settings and feedback

`printing::State` holds the transient printer selection, copies, duplex and grayscale settings,
plus discovery and submission receivers. Hosts supply `Discover` and `Submit` workers; neither
the state nor the controls invokes an OS spooler or generates PDF content. Apps map `Settings`
to `ashlar-print::Job` and own the Print action, page selection, preview and export behavior.

`State::show` takes a stable `egui::Id` and app-localized `Labels`. It draws the settings only;
apps render typed `Feedback` using their own translations. `poll` requests occasional repaints
while work is pending. Refresh disables submission until the new printer list arrives, preserves
a still-present selection, and otherwise chooses the system default or first available printer.
An empty or failed discovery cannot leave a stale printer ready to submit.

`ready` is false while a job is pending. `begin` refuses replacing an active job receiver;
dropping a receiver does not cancel a native job. Apps must test readiness before invoking the
host submit callback and keep active work alive when closing a window. Queue-default duplex is
represented by `None`, independently of explicit one-sided, long-edge or short-edge choices.
Receipts and backend errors remain unmodified data for the app to display. These controls work
on WASM too; browser PDF printing remains an app-owned service.

## Compatibility and checks

egui 0.36, Rust 1.95+. The production dependency disables egui's default features: fonts and
rendering stay the application's choice.

```sh
cargo clippy -p ashlar-ui --all-targets -- -D warnings
cargo test -p ashlar-ui
cargo test -p ashlar-example-gallery
cargo check --target wasm32-unknown-unknown -p ashlar-ui
```

The [visual regression suite](../../docs/ui/ui-visual-regressions.md) compares the
seeded controls against independent reference painters. To keep the PNGs:

```sh
ASHLAR_UI_VISUAL_DIR=/absolute/path/to/visuals cargo test -p ashlar-ui --test visual_regression
```
