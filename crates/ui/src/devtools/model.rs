//! The element tree the debugger works on: egui's own AccessKit output (every egui widget: role,
//! label, value, state, bounds) merged with ashlar's [`automation`](crate::automation) registry
//! (stable dotted ids, component roles, values egui does not expose), with short refs (`e12`) that
//! stay the same for an element across snapshots.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use egui::accesskit::{self as ak, Action, Toggled};
use egui::{Pos2, Rect};
use serde_json::{Value, json};

use crate::automation::{Element, Role};

/// Most elements one model holds (a runaway UI must not make a snapshot unbounded).
pub const MAX_NODES: usize = 50_000;
/// Deepest nesting followed.
const MAX_DEPTH: usize = 256;
/// Refs remembered before numbering starts over (old refs then read as stale).
const MAX_REFS: usize = 200_000;
/// Longest name or value shown in the text outline.
const OUTLINE_TEXT: usize = 80;

/// One element.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// What the ref is keyed by: the egui `Id` value, or a hash of the automation id.
    pub key: u64,
    /// `e<n>`.
    pub ref_n: u32,
    /// The ashlar automation id, when the element registered one.
    pub id: Option<String>,
    pub role: &'static str,
    pub name: String,
    pub value: Option<String>,
    pub enabled: bool,
    pub focused: bool,
    pub selected: Option<bool>,
    pub checked: Option<bool>,
    /// Some part of it is on screen.
    pub visible: bool,
    /// The on-screen part, in points (the whole bounds when nothing is visible).
    pub rect: Rect,
    /// Clickable, focusable or an input role.
    pub interactive: bool,
    /// The egui widget (its `Id` value), for AccessKit actions.
    pub egui: Option<u64>,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub depth: usize,
    order: f64,
    /// A label that only repeats its parent's name (left out of the outline).
    echo: bool,
}

impl Default for Node {
    fn default() -> Self {
        Node {
            key: 0,
            ref_n: 0,
            id: None,
            role: "generic",
            name: String::new(),
            value: None,
            enabled: true,
            focused: false,
            selected: None,
            checked: None,
            visible: false,
            rect: Rect::NOTHING,
            interactive: false,
            egui: None,
            parent: None,
            children: Vec::new(),
            depth: 0,
            order: 0.0,
            echo: false,
        }
    }
}

impl Node {
    pub fn ref_str(&self) -> String {
        format!("e{}", self.ref_n)
    }

    /// Where a click on it lands.
    pub fn center(&self) -> Pos2 {
        self.rect.center()
    }

    /// What `get text` reports: a text field's content, otherwise the name.
    pub fn text(&self) -> &str {
        match (self.role, &self.value) {
            ("textbox" | "searchbox", Some(v)) => v,
            _ => &self.name,
        }
    }

    pub fn to_json(&self, nodes: &[Node]) -> Value {
        let mut o = serde_json::Map::new();
        o.insert("ref".into(), json!(self.ref_str()));
        if let Some(id) = &self.id {
            o.insert("id".into(), json!(id));
        }
        o.insert("role".into(), json!(self.role));
        o.insert("name".into(), json!(self.name));
        if let Some(v) = &self.value {
            o.insert("value".into(), json!(v));
        }
        o.insert("enabled".into(), json!(self.enabled));
        o.insert("focused".into(), json!(self.focused));
        if let Some(s) = self.selected {
            o.insert("selected".into(), json!(s));
        }
        if let Some(c) = self.checked {
            o.insert("checked".into(), json!(c));
        }
        o.insert("visible".into(), json!(self.visible));
        o.insert("rect".into(), rect_json(self.rect));
        if let Some(p) = self.parent.and_then(|p| nodes.get(p)) {
            o.insert("parent".into(), json!(p.ref_str()));
        }
        Value::Object(o)
    }
}

/// `[x, y, w, h]`, to a tenth of a point.
pub fn rect_json(r: Rect) -> Value {
    let t = |v: f32| ((v * 10.0).round() / 10.0) as f64;
    json!([t(r.min.x), t(r.min.y), t(r.width()), t(r.height())])
}

/// Refs handed out so far: the same key always gets the same ref.
#[derive(Debug, Default)]
pub struct Refs {
    by_key: HashMap<u64, u32>,
    by_ref: HashMap<u32, u64>,
    next: u32,
}

impl Refs {
    fn get(&mut self, key: u64) -> u32 {
        if let Some(r) = self.by_key.get(&key) {
            return *r;
        }
        if self.by_key.len() >= MAX_REFS {
            self.by_key.clear();
            self.by_ref.clear();
        }
        self.next = self.next.wrapping_add(1).max(1);
        self.by_key.insert(key, self.next);
        self.by_ref.insert(self.next, key);
        self.next
    }

    pub fn key_of(&self, r: u32) -> Option<u64> {
        self.by_ref.get(&r).copied()
    }
}

/// The element tree of one frame.
#[derive(Debug, Default)]
pub struct Model {
    pub nodes: Vec<Node>,
    pub roots: Vec<usize>,
    /// egui pass it was built from.
    pub pass: u64,
}

/// What [`Model::build`] reads.
pub struct Sources<'a> {
    pub update: Option<&'a ak::TreeUpdate>,
    pub elements: &'a [Element],
    /// Clipped (interact) rect and enabled state of each egui widget, by `Id` value.
    pub widgets: &'a HashMap<u64, Rect>,
    pub screen: Rect,
    /// AccessKit subtrees to leave out (the inspector's own window).
    pub exclude: &'a HashSet<u64>,
    pub pass: u64,
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The role name of an AccessKit role.
pub fn ak_role(r: ak::Role) -> &'static str {
    use ak::Role as R;
    match r {
        R::Button | R::DefaultButton => "button",
        R::CheckBox => "checkbox",
        R::Switch => "switch",
        R::RadioButton => "radio",
        R::RadioGroup => "radiogroup",
        R::TextInput | R::MultilineTextInput | R::PasswordInput | R::EmailInput | R::NumberInput | R::PhoneNumberInput | R::UrlInput => "textbox",
        R::SearchInput => "searchbox",
        R::Slider => "slider",
        R::SpinButton => "spinbutton",
        R::ComboBox => "combobox",
        R::Link => "link",
        R::Label | R::Paragraph | R::TextRun => "label",
        R::Image => "image",
        R::ColorWell => "colorwell",
        R::Pane => "panel",
        R::Window => "window",
        R::Dialog | R::AlertDialog => "dialog",
        R::ScrollBar => "scrollbar",
        R::Splitter => "separator",
        R::ProgressIndicator => "progressbar",
        R::Menu => "menu",
        R::MenuBar => "menubar",
        R::MenuItem | R::MenuItemCheckBox | R::MenuItemRadio => "menuitem",
        R::Tab => "tab",
        R::TabList => "tablist",
        R::List | R::ListBox => "list",
        R::ListItem | R::ListBoxOption => "listitem",
        R::Tree => "tree",
        R::TreeItem => "treeitem",
        R::Table | R::Grid => "table",
        R::Row => "row",
        R::Cell | R::GridCell => "cell",
        R::ColumnHeader | R::RowHeader => "columnheader",
        R::Toolbar => "toolbar",
        R::Status => "status",
        R::Alert => "alert",
        R::Tooltip => "tooltip",
        R::GenericContainer | R::Group => "group",
        _ => "generic",
    }
}

/// The role name of an ashlar component role (`None` for `Other`: keep egui's).
pub fn ashlar_role(r: Role) -> Option<&'static str> {
    Some(match r {
        Role::Button | Role::Swatch => "button",
        Role::Toggle => "checkbox",
        Role::Field => "textbox",
        Role::Slider | Role::Knob => "slider",
        Role::List => "list",
        Role::Row => "listitem",
        Role::Table => "table",
        Role::Cell => "cell",
        Role::Header => "columnheader",
        Role::Tree => "tree",
        Role::TreeItem => "treeitem",
        Role::Menu => "menu",
        Role::MenuItem => "menuitem",
        Role::Dialog | Role::Palette => "dialog",
        Role::Toast => "alert",
        Role::Search => "searchbox",
        Role::Status => "status",
        Role::Tab => "tab",
        Role::Other => return None,
    })
}

const INTERACTIVE_ROLES: &[&str] = &[
    "button",
    "checkbox",
    "switch",
    "radio",
    "textbox",
    "searchbox",
    "slider",
    "spinbutton",
    "combobox",
    "link",
    "menu",
    "menuitem",
    "tab",
    "listitem",
    "treeitem",
    "colorwell",
];

fn is_input_role(role: &str) -> bool {
    INTERACTIVE_ROLES.contains(&role)
}

fn same_rect(a: Rect, b: Rect) -> bool {
    (a.min - b.min).length() <= 1.0 && (a.max - b.max).length() <= 1.0
}

fn ak_rect(n: &ak::Node) -> Option<Rect> {
    let b = n.bounds()?;
    let r = Rect::from_min_max(egui::pos2(b.x0 as f32, b.y0 as f32), egui::pos2(b.x1 as f32, b.y1 as f32));
    r.is_finite().then_some(r)
}

fn number_text(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 { format!("{}", v as i64) } else { format!("{v}") }
}

impl Model {
    /// Build the tree; `refs` keeps refs stable across builds.
    pub fn build(src: &Sources<'_>, refs: &mut Refs) -> Model {
        let mut m = Model { pass: src.pass, ..Default::default() };
        if let Some(u) = src.update {
            m.add_accesskit(u, src);
        }
        m.merge_elements(src);
        m.nest();
        m.link();
        for n in &mut m.nodes {
            n.ref_n = refs.get(n.key);
        }
        m
    }

    fn add_accesskit(&mut self, u: &ak::TreeUpdate, src: &Sources<'_>) {
        let map: HashMap<u64, &ak::Node> = u.nodes.iter().map(|(id, n)| (id.0, n)).collect();
        let root = u.tree.as_ref().map_or_else(|| egui::accesskit_root_id().value(), |t| t.root.0);
        let focus = u.focus.0;
        // (accesskit id, parent index, depth); children pushed in reverse to visit in order
        let mut stack: Vec<(u64, Option<usize>, usize)> = Vec::new();
        if let Some(r) = map.get(&root) {
            for c in r.children().iter().rev() {
                stack.push((c.0, None, 1));
            }
        }
        while let Some((id, parent, depth)) = stack.pop() {
            if self.nodes.len() >= MAX_NODES {
                break;
            }
            let Some(n) = map.get(&id) else { continue };
            if n.is_hidden() {
                continue;
            }
            let role = ak_role(n.role());
            let mut name = n.label().unwrap_or_default().to_string();
            let mut value = n.value().map(str::to_string).or_else(|| n.numeric_value().map(number_text));
            if role == "label" && name.is_empty() {
                name = value.take().unwrap_or_default();
            }
            if name.is_empty()
                && matches!(role, "textbox" | "searchbox")
                && let Some(p) = n.placeholder()
            {
                name = p.to_string();
            }
            // egui labels are selectable text (clickable); that does not make them controls
            let actionable = n.supports_action(Action::Click) || n.supports_action(Action::Focus);
            let interactive = is_input_role(role) || (actionable && !matches!(role, "label" | "image"));
            // the text run egui puts inside a text field or a label repeats its parent
            let repeats = role == "label"
                && parent.and_then(|p| self.nodes.get(p)).is_some_and(|p| matches!(p.role, "textbox" | "searchbox") || p.name == name);
            if repeats || src.exclude.contains(&id) {
                continue;
            }
            let keep = !(matches!(role, "group" | "generic") && name.is_empty() && !interactive);
            let mut next_parent = parent;
            if keep {
                let full = ak_rect(n).unwrap_or(Rect::NOTHING);
                let clipped = src.widgets.get(&id).copied().unwrap_or(full).intersect(src.screen);
                let visible = clipped.is_positive();
                let (mut checked, mut selected) = (None, n.is_selected());
                let toggled = n.toggled().map(|t| t == Toggled::True);
                if matches!(role, "checkbox" | "radio" | "switch" | "menuitem") {
                    checked = toggled;
                } else if selected.is_none() {
                    selected = toggled;
                }
                self.nodes.push(Node {
                    // egui's automatic ids are positional: a widget removed before this one hands
                    // its id on, so the role is part of what keeps a ref on the same element
                    key: id ^ fnv(role),
                    role,
                    name,
                    value,
                    enabled: !n.is_disabled(),
                    focused: id == focus && id != root,
                    selected,
                    checked,
                    visible,
                    rect: if visible { clipped } else { full },
                    interactive,
                    egui: Some(id),
                    parent,
                    order: self.nodes.len() as f64,
                    ..Default::default()
                });
                next_parent = Some(self.nodes.len() - 1);
            }
            if depth < MAX_DEPTH {
                for c in n.children().iter().rev() {
                    stack.push((c.0, next_parent, depth + 1));
                }
            }
        }
    }

    /// Fold ashlar's registered elements in: onto egui's node for the same widget, or as nodes of
    /// their own under the element that contains them.
    fn merge_elements(&mut self, src: &Sources<'_>) {
        for e in src.elements {
            if self.nodes.len() >= MAX_NODES {
                break;
            }
            let rect = e.egui_rect();
            let found = e
                .egui_id
                .and_then(|g| self.nodes.iter().position(|n| n.egui == Some(g) && n.id.is_none()))
                .or_else(|| self.nodes.iter().rposition(|n| n.id.is_none() && n.interactive && same_rect(n.rect, rect)));
            let role = ashlar_role(e.role);
            if let Some(n) = found.and_then(|i| self.nodes.get_mut(i)) {
                n.key = fnv(&e.id) | (1 << 63);
                n.id = Some(e.id.clone());
                if let Some(r) = role {
                    n.role = r;
                }
                if !e.label.is_empty() {
                    n.name.clone_from(&e.label);
                }
                if !e.value.is_empty() {
                    n.value = Some(e.value.clone());
                }
                if e.role == Role::Toggle {
                    n.checked = e.selected.or(n.checked);
                } else if e.selected.is_some() {
                    n.selected = e.selected;
                }
                n.enabled &= e.enabled;
                n.interactive |= role.is_some_and(is_input_role);
                continue;
            }
            // its own node, under the deepest node containing it; siblings inside it move under it
            let parent =
                self.nodes.iter().enumerate().filter(|(_, n)| n.rect.contains_rect(rect)).max_by_key(|(_, n)| n.depth_hint()).map(|(i, _)| i);
            let idx = self.nodes.len();
            let mut first_child: Option<f64> = None;
            for n in &mut self.nodes {
                if n.parent == parent && rect.contains_rect(n.rect) && n.rect.is_positive() {
                    n.parent = Some(idx);
                    first_child = Some(first_child.map_or(n.order, |o: f64| o.min(n.order)));
                }
            }
            let role = role.unwrap_or("generic");
            let order = first_child.map_or(idx as f64, |o| o - 0.5);
            self.nodes.push(Node {
                key: fnv(&e.id) | (1 << 63),
                id: Some(e.id.clone()),
                role,
                name: e.label.clone(),
                value: (!e.value.is_empty()).then(|| e.value.clone()),
                enabled: e.enabled,
                selected: if e.role == Role::Toggle { None } else { e.selected },
                checked: if e.role == Role::Toggle { e.selected } else { None },
                visible: rect.is_positive(),
                rect,
                interactive: is_input_role(role),
                parent,
                order,
                ..Default::default()
            });
        }
    }

    /// egui's tree is flat where ashlar's components draw their own rows: a list, row, dialog…
    /// that ashlar registered takes the later siblings drawn inside its rect as children.
    fn nest(&mut self) {
        const CONTAINERS: &[&str] = &[
            "list", "listitem", "tree", "treeitem", "table", "row", "dialog", "menu", "menubar", "toolbar", "tablist", "status", "panel", "window",
            "group",
        ];
        let mut by_order: Vec<usize> = (0..self.nodes.len()).collect();
        by_order.sort_by(|a, b| {
            let o = |i: &usize| self.nodes.get(*i).map_or(0.0, |n| n.order);
            o(a).total_cmp(&o(b))
        });
        let mut budget: usize = 4_000_000;
        for (pos, &c) in by_order.iter().enumerate() {
            let Some(container) = self.nodes.get(c) else { continue };
            if container.id.is_none() || !CONTAINERS.contains(&container.role) || !container.rect.is_positive() {
                continue;
            }
            let (parent, rect, name) = (container.parent, container.rect, container.name.clone());
            for &j in by_order.get(pos + 1..).unwrap_or(&[]) {
                budget = budget.saturating_sub(1);
                if budget == 0 {
                    return;
                }
                if let Some(n) = self.nodes.get_mut(j)
                    && n.parent == parent
                    && n.rect.is_positive()
                    && rect.contains_rect(n.rect)
                {
                    n.parent = Some(c);
                    n.echo = n.role == "label" && !name.is_empty() && n.name == name;
                }
            }
        }
    }

    /// Children lists (in order) and depths from the parent links.
    fn link(&mut self) {
        let n = self.nodes.len();
        let mut kids: Vec<Vec<usize>> = vec![Vec::new(); n];
        self.roots.clear();
        for (i, node) in self.nodes.iter().enumerate() {
            match node.parent.and_then(|p| kids.get_mut(p)) {
                Some(k) => k.push(i),
                None => self.roots.push(i),
            }
        }
        let order = |i: &usize| self.nodes.get(*i).map_or(0.0, |n| n.order);
        for k in &mut kids {
            k.sort_by(|a, b| order(a).total_cmp(&order(b)));
        }
        let mut roots = std::mem::take(&mut self.roots);
        roots.sort_by(|a, b| order(a).total_cmp(&order(b)));
        let mut stack: Vec<(usize, usize)> = roots.iter().rev().map(|r| (*r, 0)).collect();
        let mut seen = vec![false; n];
        while let Some((i, d)) = stack.pop() {
            if seen.get(i).copied().unwrap_or(true) {
                continue;
            }
            if let Some(s) = seen.get_mut(i) {
                *s = true;
            }
            for c in kids.get(i).into_iter().flatten().rev() {
                stack.push((*c, d + 1));
            }
            if let Some(node) = self.nodes.get_mut(i) {
                node.depth = d;
            }
        }
        for (node, k) in self.nodes.iter_mut().zip(kids) {
            node.children = k;
        }
        self.roots = roots;
    }

    pub fn get(&self, i: usize) -> Option<&Node> {
        self.nodes.get(i)
    }

    /// Nodes in reading order (pre-order), starting at `scope` or the roots.
    pub fn preorder(&self, scope: Option<usize>) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack: Vec<usize> = match scope {
            Some(s) => vec![s],
            None => self.roots.iter().rev().copied().collect(),
        };
        while let Some(i) = stack.pop() {
            if out.len() > MAX_NODES {
                break;
            }
            out.push(i);
            if let Some(n) = self.nodes.get(i) {
                stack.extend(n.children.iter().rev());
            }
        }
        out
    }

    pub fn by_key(&self, key: u64) -> Option<usize> {
        self.nodes.iter().position(|n| n.key == key)
    }

    /// The smallest visible element under `p`.
    pub fn at(&self, p: Pos2) -> Option<usize> {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.visible && n.rect.contains(p))
            .min_by(|(_, a), (_, b)| a.rect.area().total_cmp(&b.rect.area()).then(b.depth.cmp(&a.depth)))
            .map(|(i, _)| i)
    }

    /// Whether any visible element shows `text` (in its name or value).
    pub fn shows_text(&self, text: &str) -> bool {
        self.nodes.iter().any(|n| n.visible && (n.name.contains(text) || n.value.as_deref().is_some_and(|v| v.contains(text))))
    }

    /// The text outline (`- button "Save" [ref=e3]`) and the nodes it lists.
    pub fn outline(&self, opts: &Outline) -> (String, Vec<usize>) {
        let mut s = String::new();
        let mut listed = Vec::new();
        // (node, printed depth)
        let mut stack: Vec<(usize, usize)> = match opts.scope {
            Some(i) => vec![(i, 0)],
            None => self.roots.iter().rev().map(|r| (*r, 0)).collect(),
        };
        while let Some((i, d)) = stack.pop() {
            let Some(n) = self.nodes.get(i) else { continue };
            if listed.len() >= MAX_NODES {
                break;
            }
            let shown = opts.shows(n);
            let child_d = if shown { d + 1 } else { d };
            if shown {
                listed.push(i);
                line(&mut s, n, d);
            }
            if opts.depth.is_none_or(|max| child_d <= max) {
                for c in n.children.iter().rev() {
                    stack.push((*c, child_d));
                }
            }
        }
        (s, listed)
    }
}

impl Node {
    fn depth_hint(&self) -> usize {
        // before `link`, nodes from the AccessKit walk are in pre-order: a later container that
        // contains the rect is nested deeper (or is a later sibling, which is as good a parent)
        self.order as usize
    }
}

/// What a snapshot lists.
#[derive(Clone, Copy, Debug, Default)]
pub struct Outline {
    /// Only clickable / focusable / input elements.
    pub interactive: bool,
    /// Leave out unnamed, non-interactive elements and everything off screen.
    pub compact: bool,
    pub depth: Option<usize>,
    pub scope: Option<usize>,
}

impl Outline {
    fn shows(&self, n: &Node) -> bool {
        if n.echo {
            return false;
        }
        if self.interactive {
            return n.interactive && n.visible;
        }
        if self.compact {
            return n.visible && (n.interactive || !n.name.is_empty() || n.id.is_some());
        }
        true
    }
}

fn quoted(s: &mut String, text: &str) {
    s.push('"');
    for (i, c) in text.chars().enumerate() {
        if i >= OUTLINE_TEXT {
            s.push('…');
            break;
        }
        match c {
            '"' => s.push_str("\\\""),
            '\n' => s.push_str("\\n"),
            c if c.is_control() => {}
            c => s.push(c),
        }
    }
    s.push('"');
}

fn line(s: &mut String, n: &Node, depth: usize) {
    for _ in 0..depth.min(64) {
        s.push_str("  ");
    }
    let _ = write!(s, "- {}", n.role);
    if !n.name.is_empty() {
        s.push(' ');
        quoted(s, &n.name);
    }
    let _ = write!(s, " [ref=e{}]", n.ref_n);
    if let Some(id) = &n.id {
        let _ = write!(s, " [id={id}]");
    }
    if let Some(v) = n.value.as_deref().filter(|v| *v != n.name) {
        s.push_str(" [value=");
        quoted(s, v);
        s.push(']');
    }
    if !n.enabled {
        s.push_str(" [disabled]");
    }
    if n.focused {
        s.push_str(" [focused]");
    }
    if n.selected == Some(true) {
        s.push_str(" [selected]");
    }
    match n.checked {
        Some(true) => s.push_str(" [checked]"),
        Some(false) => s.push_str(" [unchecked]"),
        None => {}
    }
    if !n.visible {
        s.push_str(" [offscreen]");
    }
    s.push('\n');
}

// ---------------------------------------------------------------------------------------------
// Targets
// ---------------------------------------------------------------------------------------------

/// What a request acts on.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Ref(u32),
    Id(String),
    Text { text: String, role: Option<String> },
    Point(Pos2),
}

fn parse_ref(s: &str) -> Option<u32> {
    s.trim().trim_start_matches('@').strip_prefix('e')?.parse().ok()
}

impl Target {
    /// `{ref}` | `{id}` | `{text, role?}` | `{x, y}`, or a bare `"@e3"` / `"e3"` string.
    pub fn parse(v: &Value) -> Option<Target> {
        if let Some(s) = v.as_str() {
            return parse_ref(s).map(Target::Ref);
        }
        if let Some(r) = v.get("ref") {
            return r.as_str().and_then(parse_ref).or_else(|| r.as_u64().and_then(|n| u32::try_from(n).ok())).map(Target::Ref);
        }
        if let Some(id) = v.get("id").and_then(Value::as_str) {
            return Some(Target::Id(id.to_string()));
        }
        if let Some(t) = v.get("text").and_then(Value::as_str) {
            return Some(Target::Text { text: t.to_string(), role: v.get("role").and_then(Value::as_str).map(str::to_string) });
        }
        super::input::point(v).map(Target::Point)
    }

    /// The target of a request: `params.target`, or the target fields given flat in `params`.
    pub fn of(p: &Value) -> Option<Target> {
        match p.get("target") {
            Some(t) if !t.is_null() => Target::parse(t),
            _ => Target::parse_flat(p),
        }
    }

    /// Like [`Target::parse`], but `text` alone is not a target here (`ui.fill {text}` is the
    /// text to type).
    fn parse_flat(p: &Value) -> Option<Target> {
        if p.get("ref").is_some() || p.get("id").is_some() || p.get("x").is_some() {
            return Target::parse(p);
        }
        None
    }

    pub fn describe(&self) -> String {
        match self {
            Target::Ref(r) => format!("@e{r}"),
            Target::Id(id) => format!("id `{id}`"),
            Target::Text { text, role: Some(r) } => format!("{r} \"{text}\""),
            Target::Text { text, role: None } => format!("\"{text}\""),
            Target::Point(p) => format!("({}, {})", p.x, p.y),
        }
    }
}

/// A target that matched nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct Miss {
    pub error: String,
    pub detail: Value,
}

/// What a target resolved to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub node: Option<usize>,
    pub point: Pos2,
}

impl Model {
    /// Find `t` in this model. `generation` is the latest snapshot's, for the stale-ref error.
    pub fn resolve(&self, t: &Target, refs: &Refs, generation: u64) -> Result<Hit, Miss> {
        let hit = |i: usize| self.nodes.get(i).map(|n| Hit { node: Some(i), point: n.center() });
        let not_found = || Miss {
            error: format!("not_found: no element {} on screen (take a new ui.snapshot)", t.describe()),
            detail: json!({"kind": "not_found", "generation": generation}),
        };
        match t {
            Target::Ref(r) => {
                let found = refs.key_of(*r).and_then(|k| self.by_key(k));
                match found.and_then(hit) {
                    Some(h) => Ok(h),
                    None => Err(Miss {
                        error: format!("stale_ref: @e{r} is gone (snapshot generation {generation}; take a new ui.snapshot)"),
                        detail: json!({"kind": "stale_ref", "ref": format!("e{r}"), "generation": generation}),
                    }),
                }
            }
            Target::Id(id) => {
                let i = self.nodes.iter().position(|n| n.visible && n.id.as_deref() == Some(id.as_str()));
                let i = i.or_else(|| self.nodes.iter().position(|n| n.id.as_deref() == Some(id.as_str())));
                i.and_then(hit).ok_or_else(not_found)
            }
            Target::Text { text, role } => self.find_text(text, role.as_deref()).and_then(hit).ok_or_else(not_found),
            Target::Point(p) => Ok(Hit { node: self.at(*p), point: *p }),
        }
    }

    /// The first visible element named `text`: exact, then ignoring case, then containing it;
    /// interactive ones first within each tier.
    pub fn find_text(&self, text: &str, role: Option<&str>) -> Option<usize> {
        let lower = text.to_lowercase();
        let order = self.preorder(None);
        let candidates =
            || order.iter().filter_map(|i| self.nodes.get(*i).map(|n| (*i, n))).filter(|(_, n)| n.visible && role.is_none_or(|r| n.role == r));
        let tiers: [&dyn Fn(&Node) -> bool; 3] =
            [&|n: &Node| n.name == text, &|n: &Node| n.name.to_lowercase() == lower, &|n: &Node| n.name.to_lowercase().contains(&lower)];
        for tier in tiers {
            let mut first = None;
            for (i, n) in candidates() {
                if tier(n) {
                    if n.interactive {
                        return Some(i);
                    }
                    first = first.or(Some(i));
                }
            }
            if first.is_some() {
                return first;
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    fn ak_node(role: ak::Role, label: Option<&str>, bounds: [f64; 4], children: &[u64]) -> ak::Node {
        let mut n = ak::Node::new(role);
        if let Some(l) = label {
            n.set_label(l);
        }
        n.set_bounds(ak::Rect { x0: bounds[0], y0: bounds[1], x1: bounds[2], y1: bounds[3] });
        n.set_children(children.iter().map(|c| ak::NodeId(*c)).collect::<Vec<_>>());
        n
    }

    fn update() -> ak::TreeUpdate {
        let root = egui::accesskit_root_id().value();
        let mut button = ak_node(ak::Role::Button, Some("Save"), [10.0, 10.0, 60.0, 30.0], &[]);
        button.add_action(Action::Click);
        let mut label = ak_node(ak::Role::Label, None, [10.0, 40.0, 60.0, 60.0], &[]);
        label.set_value("3 notes");
        let mut row = ak_node(ak::Role::Unknown, None, [0.0, 100.0, 200.0, 122.0], &[]);
        row.add_action(Action::Click);
        let mut gone = ak_node(ak::Role::Button, Some("Hidden"), [0.0, 0.0, 1.0, 1.0], &[]);
        gone.set_hidden();
        let mut check = ak_node(ak::Role::CheckBox, Some("Wrap"), [10.0, 70.0, 80.0, 90.0], &[]);
        check.set_toggled(Toggled::True);
        check.set_disabled();
        ak::TreeUpdate {
            nodes: vec![
                (ak::NodeId(root), ak_node(ak::Role::Window, None, [0.0, 0.0, 400.0, 300.0], &[2])),
                (ak::NodeId(2), ak_node(ak::Role::GenericContainer, None, [0.0, 0.0, 400.0, 300.0], &[3, 4, 5, 6, 7])),
                (ak::NodeId(3), button),
                (ak::NodeId(4), label),
                (ak::NodeId(5), row),
                (ak::NodeId(6), gone),
                (ak::NodeId(7), check),
            ],
            tree: Some(ak::Tree::new(ak::NodeId(root))),
            tree_id: ak::TreeId::ROOT,
            focus: ak::NodeId(3),
        }
    }

    fn element(id: &str, label: &str, role: Role, rect: Rect, egui_id: Option<u64>) -> Element {
        Element {
            id: id.into(),
            label: label.into(),
            role,
            rect: [rect.min.x, rect.min.y, rect.width(), rect.height()],
            enabled: true,
            egui_id,
            ..Default::default()
        }
    }

    fn build(refs: &mut Refs, elements: &[Element]) -> Model {
        let u = update();
        let widgets = HashMap::new();
        let src = Sources {
            update: Some(&u),
            elements,
            widgets: &widgets,
            screen: Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0)),
            exclude: &HashSet::new(),
            pass: 1,
        };
        Model::build(&src, refs)
    }

    #[test]
    fn accesskit_and_automation_merge_into_one_tree() {
        let mut refs = Refs::default();
        let row = Rect::from_min_max(pos2(0.0, 100.0), pos2(200.0, 122.0));
        let elements = [
            element("toolbar.save", "Save", Role::Button, Rect::from_min_max(pos2(10.0, 10.0), pos2(60.0, 30.0)), Some(3)),
            element("list.row.0", "First", Role::Row, row, None),
            element("status.text", "Ready", Role::Status, Rect::from_min_max(pos2(0.0, 280.0), pos2(400.0, 300.0)), None),
        ];
        let m = build(&mut refs, &elements);
        let (text, listed) = m.outline(&Outline::default());
        assert_eq!(
            text,
            "- button \"Save\" [ref=e1] [id=toolbar.save] [focused]\n- label \"3 notes\" [ref=e2]\n- listitem \"First\" [ref=e3] [id=list.row.0]\n- checkbox \"Wrap\" [ref=e4] [disabled] [checked]\n- status \"Ready\" [ref=e5] [id=status.text]\n"
        );
        assert_eq!(listed.len(), 5);
        let (i, _) = m.outline(&Outline { interactive: true, ..Default::default() });
        assert!(!i.contains("label"), "{i}");
        // refs survive a rebuild; a new element gets a new one
        let m2 = build(&mut refs, &elements[..2]);
        assert_eq!(m2.outline(&Outline::default()).0.lines().next(), Some("- button \"Save\" [ref=e1] [id=toolbar.save] [focused]"));
        let save = m2.resolve(&Target::Ref(1), &refs, 2).unwrap();
        assert_eq!(save.point, pos2(35.0, 20.0));
        let stale = m2.resolve(&Target::Ref(5), &refs, 2).unwrap_err();
        assert_eq!(stale.detail["kind"], "stale_ref");
        assert!(stale.error.contains("generation 2"), "{}", stale.error);
        assert!(m2.resolve(&Target::Ref(999), &refs, 2).is_err());
        let json = m2.get(0).unwrap().to_json(&m2.nodes);
        assert_eq!(json["ref"], "e1");
        assert_eq!(json["rect"], json!([10.0, 10.0, 50.0, 20.0]));
    }

    #[test]
    fn targets_resolve_by_id_text_and_point() {
        let mut refs = Refs::default();
        let m = build(&mut refs, &[element("toolbar.save", "Save", Role::Button, Rect::from_min_max(pos2(10.0, 10.0), pos2(60.0, 30.0)), Some(3))]);
        let r = |t: Target| m.resolve(&t, &refs, 1).map(|h| h.node.and_then(|i| m.get(i)).map(|n| n.name.clone()));
        assert_eq!(r(Target::Id("toolbar.save".into())), Ok(Some("Save".into())));
        assert_eq!(r(Target::Text { text: "save".into(), role: None }), Ok(Some("Save".into())));
        assert_eq!(r(Target::Text { text: "notes".into(), role: None }), Ok(Some("3 notes".into())));
        assert!(r(Target::Text { text: "Save".into(), role: Some("checkbox".into()) }).is_err());
        assert_eq!(r(Target::Point(pos2(20.0, 20.0))), Ok(Some("Save".into())));
        assert!(m.shows_text("3 notes"));
        assert!(!m.shows_text("Hidden"));
    }

    #[test]
    fn target_params_parse() {
        assert_eq!(Target::parse(&json!("@e7")), Some(Target::Ref(7)));
        assert_eq!(Target::of(&json!({"target": {"ref": "e3"}})), Some(Target::Ref(3)));
        assert_eq!(Target::of(&json!({"ref": 4})), Some(Target::Ref(4)));
        assert_eq!(Target::of(&json!({"id": "a.b"})), Some(Target::Id("a.b".into())));
        assert_eq!(Target::of(&json!({"x": 1, "y": 2})), Some(Target::Point(pos2(1.0, 2.0))));
        assert_eq!(Target::of(&json!({"text": "typed, not a target"})), None);
        assert_eq!(
            Target::of(&json!({"target": {"text": "Save", "role": "button"}})),
            Some(Target::Text { text: "Save".into(), role: Some("button".into()) })
        );
        assert_eq!(Target::parse(&json!({"ref": "x9"})), None);
        assert_eq!(Target::parse(&json!(5)), None);
    }
}
