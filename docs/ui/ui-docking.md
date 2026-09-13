# Panel docking

`ashlar_ui::docking` arranges an app's panels: it moves one panel at a time between docked tab
groups, side-by-side splits and floating groups. The library owns placement and the gestures that
change it. The app owns everything inside a panel, its commands, its documents, and the policy for
saving and loading layouts.

## The model

A `Layout<P>` is an optional docked tree (`root`) plus a list of floating groups, back to front.
The tree has three node kinds:

- `Node::Tabs { panels, active }`: a tab group, its panels in order and the index of the one shown.
- `Node::Split { axis, size, first, second }`: two children side by side (`Horizontal`) or stacked
  (`Vertical`). `size` is a `SplitSize`: a ratio of the space, a fixed first size, or a fixed
  second size.
- `Node::Stack { entries }`: an accordion. Each `StackEntry` has a panel, an `open` flag and an
  optional preferred body height.

A `Floating<P>` is a tab group with a rect `[x, y, width, height]`.

`P` is the app's panel id. It must be stable across sessions and languages (an enum or a
string key, never a translated title) and implement `Clone + Eq + Hash`; the renderer also
needs `Debug + Send + Sync + 'static`. A panel appears at most once. Per-document tabs (one tab per
open file, say) belong inside a panel, not in the layout.

```rust
use ashlar_ui::docking::{Action, DockError, Floating, Layout, Node, Placement};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Panel { Outline, Inspector, History }

fn arrange() -> Result<Layout<Panel>, DockError> {
    let mut layout = Layout {
        root: Some(Node::Tabs { panels: vec![Panel::Outline, Panel::Inspector], active: 0 }),
        floating: vec![Floating { panels: vec![Panel::History], active: 0, rect: [40.0, 60.0, 260.0, 300.0] }],
    };
    // Dock History into the main group, in front of Inspector.
    layout.apply(Action::Move {
        panel: Panel::History,
        anchor: Panel::Outline,
        placement: Placement::Tab { before: Some(Panel::Inspector) },
    })?;
    Ok(layout)
}
```

## Transactions

Every change is an `Action`, applied with `Layout::apply` or `Layout::apply_with_permissions`.
A transaction checks the current layout, makes the change on a copy, checks the copy, and only
then replaces the layout. When it fails, the layout is exactly as before and a `DockError` says
why: a missing panel or target, a panel moved onto itself, a duplicate id, an active index out of
range, too deep or too many (`MAX_DEPTH`, `MAX_PANELS`), or geometry that is not finite or out of
bounds.

What a successful move guarantees:

- the moved panel is never lost on the way;
- each group keeps its selected panel by identity, not by index;
- groups left empty disappear, and a split left with one child is replaced by that child;
- an accordion entry keeps its open state and height when it moves to another accordion;
- sizes of splits the move did not touch stay as they were.

`Open` adds a panel that is not in the layout yet (or activates it if it is). `OpenAt` places a
new panel at an exact spot, and moves it there if it already exists. Whether a panel id is
known to the app at all is for the app's panel registry to check before it asks.

## Permissions and fixed panels

`apply_with_permissions` takes a function from panel to `Permissions` (`movable`, `floatable`,
`closable`, `accepts_tabs`). A document canvas is typically `Permissions::PROTECTED`: it cannot be
moved, closed or floated, and nothing can join it as a tab, but other panels can still be split
off beside it. A panel that must never leave the docked tree is the app's rule to enforce.
An empty root (`root: None`, or a `Tabs` node with no panels) is valid, for example when every
panel floats.

## Rendering

Apps that draw their own chrome can describe their groups as `DropTarget`s and call
`resolve_drop` with the pointer. Targets are listed back to front, floating groups in their
current stacking order after the docked ones. The frontmost group under the pointer decides
alone: if it refuses the drop, groups behind it are not considered. `drop_zone` and `zone_rect`
give the zone rules on their own.

`DockArea` is the optional built-in renderer. It fits tabs into the strip and moves the rest into
an overflow menu, previews drops, cancels drags on Escape or when the window loses focus, and
draws split and accordion dividers and the floating groups' resize corners. It never changes the
layout: `DockOutput::actions` lists what the user asked for, in order, for the app to send
through its own command path. With the `serde` feature `Action` serializes, so pointer gestures,
keyboard commands and remote control can all go through the same path. Panel bodies get an egui
id derived from the dock area and the panel only, so their widget state follows a panel when it
moves; durable data should still live in the app, not in egui memory.

`DockStyle` holds the colours, font, metrics and the localized labels of the stock controls
(`DockStyle::from_ui` gives defaults). `show_with_limits` takes per-panel `PanelLimits`. The
renderer fits the view into the viewport: when there is not enough room, minimums shrink in
proportion, and the preferred sizes stored in the layout are never overwritten by that. Divider
gestures produce explicit `ResizeSplit` or `ResizeStack` actions.

A custom header that starts dragging a panel can hand the gesture to the renderer with
`begin_drag`, using the same area id.

Floating groups are egui areas inside one viewport. Native windows, moving panels between
viewports, and monitor coordinates are for the host to build.

## Persistence

The `serde` feature derives serialization for the model; apps can also write their own adapter.
The library picks no file location. Before trusting a loaded layout, validate panel ids against the
app's registry, cap the payload size, version the schema, and bound the depth of the
deserializer: `validate` rejects deep trees, but it cannot protect a parser that has already
recursed without limit.

## Hiding and restoring panels

To hide a panel, capture `layout.location(&panel)`, apply `Close`, and keep the `Location`
alongside the app's own visibility state. `restore(panel, &location)` puts it back using its
neighbours' ids, fixed split sizes, the floating rect, and recreates a closed accordion entry.
If the neighbour is gone, the layout is left unchanged and `MissingTarget` is returned; the app
then picks a fallback such as `Open`. Panels hidden from the same group come back correctly when
restored in reverse order of closing.

## What the tests cover

The module's unit tests cover moves, floats and merges, rejected transactions, selection by
identity, reordering, accordion state, permissions, location and restore for splits, floating
groups and accordions, validation limits, drop resolution, 800 random moves, serde round trips,
and the renderer's gestures: drags with Escape and focus loss, size limits, the resize corner,
accordion dividers, custom content, blockers, `cancel_drag` ownership, `chrome_has_focus`, and
accessible names of the stock menu actions.

`crates/ui/tests/docking_workflows.rs` drives a document canvas and three populated panels with
real pointer and keyboard input: it edits a value, merges a floating panel into a tab group,
detaches it again and redocks it, at two widths, two scales and both themes, round-tripping every
action through JSON. Set `ASHLAR_UI_DOCK_VISUAL_DIR` to an absolute path to keep a PNG per stage:

```sh
ASHLAR_UI_DOCK_VISUAL_DIR=/abs/path cargo test -p ashlar-ui --all-features --test docking_workflows
```

These checks run offscreen. They do not replace an app's own tests of its panels and commands.

## Custom chrome and content

`show_customized` takes one `DockContent` adapter instead of closures: `body` draws a panel,
`panel_menu` adds entries at the top of a tab's context menu, and `group_header_width` /
`group_header` reserve and draw a strip at the right end of a group's tab bar (clipped to it).
Queue commands from these callbacks rather than changing the layout inside them. Collapsed
accordion entries still have their context menu. Collapsing whole tab groups is a host policy.

`label` returns `WidgetText`, so a tab can show a shaped galley (icons, mixed styles); the same
text is used to measure the tab, to paint it and in menus. The plain string from the `label`
closure stays the accessible name. `action_label` shapes the text of the stock Float, Move and
Close entries; it gets no panel id. Visible text never replaces an accessible name. `show` and
`show_with_limits` remain for apps without an adapter.

## Activation and focus

To bring a panel forward, apply `Activate` and pass `raise_panel(panel)` on the next frame: a
floating group is raised and the panel's tab takes keyboard focus. Stacking order is not stored
in the layout. A floating group is identified by its first panel.

The 12-point grip at the left of a floating group's tab strip moves the whole group with
`MoveFloating`; dragging a tab detaches just that panel. `DockOutput::group_drag` and
`group_drop` report which group is being dragged and where it was released, for hosts that turn a
drop outside the window into a native window, say. Escape or focus loss puts the group back where
it started.

`blocked_rects` marks screen areas (menus, dialogs, overlays) where previews, drops and the float
fallback are blocked, including overlays that appear in the middle of a drag. Hosts pass the
current overlay geometry every frame, or call `cancel_drag` while something modal is up.

`DockArea::chrome_has_focus` says whether keyboard focus is on the dock's own controls: tabs,
collapse and overflow buttons, dividers, resize corners and grips. Panel content and custom
header or menu widgets do not count, and an invalid layout gives `false`. A host can use it to let
global shortcuts through while Enter, Space, Tab, Escape and the arrow keys stay with the focused
control.
