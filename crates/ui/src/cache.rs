//! Revision-keyed caching of galleys, meshes and other derived paint data across frames.
//!
//! A cached value is identified by a key (what it is: a row id, a label's widget id) and is valid
//! while its revision (what it was built from: a document revision, a value's bits) is
//! unchanged. Nothing is diffed: the caller says when the input changed by changing the revision.
//! Entries unused for a number of frames are evicted; a full cache drops its least recently used
//! half, never the entries in use this frame while older ones remain.
//!
//! ```
//! # use ashlar_ui::cache;
//! # let ctx = egui::Context::default();
//! # let mut out = ctx.run_ui(Default::default(), |ui| {
//! let doc_revision = 7;
//! let galley = cache::galley(ui, ("row-label", 42), doc_revision, || {
//!     egui::text::LayoutJob::simple_singleline("Chapter 42".into(), egui::FontId::proportional(13.0), egui::Color32::WHITE)
//! });
//! ui.painter().galley(ui.max_rect().min, galley, egui::Color32::WHITE);
//! # });
//! # out.textures_delta.clear();
//! ```

use std::collections::HashMap;
use std::collections::hash_map::Entry as MapEntry;
use std::hash::Hash;
use std::sync::Arc;

use egui::{Galley, Mesh, text::LayoutJob};

/// The hasher of every cache here: fast, and with fixed seeds, so a value hashes the same in every
/// run (keys are not attacker-chosen: they are widget ids and revisions).
fn hasher() -> ahash::RandomState {
    ahash::RandomState::with_seeds(0x6173_686c, 0x6172_5f75, 0x695f_6361, 0x6368_6573)
}

/// Hash anything into a revision or key value.
pub fn hash_of(value: &impl Hash) -> u64 {
    hasher().hash_one(value)
}

struct Entry<V> {
    revision: u64,
    last_used: u64,
    value: V,
}

/// Hit and miss counts since creation (or the last [`RevisionCache::reset_stats`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub hits: u64,
    pub misses: u64,
}

/// Values keyed by `K`, each valid for one revision, evicted after `keep_frames` unused frames.
pub struct RevisionCache<K, V> {
    entries: HashMap<K, Entry<V>, ahash::RandomState>,
    frame: u64,
    keep_frames: u64,
    max_entries: usize,
    stats: Stats,
}

impl<K: Eq + Hash, V> RevisionCache<K, V> {
    /// Keep entries for `keep_frames` frames after their last use, and at most `max_entries`.
    pub fn new(keep_frames: u64, max_entries: usize) -> Self {
        Self { entries: HashMap::with_hasher(hasher()), frame: 0, keep_frames, max_entries: max_entries.max(1), stats: Stats::default() }
    }

    /// The value for `key` if it was built for `revision`.
    pub fn get(&mut self, key: &K, revision: u64) -> Option<&V> {
        let frame = self.frame;
        match self.entries.get_mut(key) {
            Some(e) if e.revision == revision => {
                e.last_used = frame;
                self.stats.hits += 1;
                Some(&e.value)
            }
            _ => None,
        }
    }

    /// The value for `key` at `revision`, building it with `make` when missing or stale.
    pub fn get_or_insert_with(&mut self, key: K, revision: u64, make: impl FnOnce() -> V) -> &V {
        let frame = self.frame;
        if self.entries.len() >= self.max_entries && !self.entries.contains_key(&key) {
            self.make_room();
        }
        match self.entries.entry(key) {
            MapEntry::Occupied(o) => {
                let e = o.into_mut();
                if e.revision == revision {
                    self.stats.hits += 1;
                } else {
                    self.stats.misses += 1;
                    e.value = make();
                    e.revision = revision;
                }
                e.last_used = frame;
                &e.value
            }
            MapEntry::Vacant(v) => {
                self.stats.misses += 1;
                &v.insert(Entry { revision, last_used: frame, value: make() }).value
            }
        }
    }

    /// Advance one frame and evict entries unused for `keep_frames` frames.
    pub fn end_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        let oldest = self.frame.saturating_sub(self.keep_frames);
        // sweeping is O(entries); do it only every few frames
        if self.frame.is_multiple_of(self.keep_frames.clamp(1, 64)) {
            self.evict_older_than(oldest);
        }
    }

    fn evict_older_than(&mut self, frame: u64) {
        self.entries.retain(|_, e| e.last_used >= frame);
    }

    /// The cache is full: drop about the least recently used half. Entries last used in the same
    /// frame go together, so the ones in use stay while older ones remain; only when nearly all of
    /// them were used this frame (more entries in use than fit) does it drop half of those.
    /// O(entries), once every `max_entries / 2` inserts at most.
    fn make_room(&mut self) {
        let len = self.entries.len();
        let mut used: Vec<u64> = self.entries.values().map(|e| e.last_used).collect();
        let (_, &mut cutoff, _) = used.select_nth_unstable(len / 2);
        // everything older than the median frame
        self.entries.retain(|_, e| e.last_used >= cutoff);
        if self.entries.len() + self.max_entries / 8 <= len && self.entries.len() < self.max_entries {
            return;
        }
        // the median frame's entries too, unless that is the current frame
        if cutoff < self.frame {
            self.entries.retain(|_, e| e.last_used > cutoff);
        }
        // more entries in use than fit: drop any half rather than grow
        if self.entries.len() + self.max_entries / 8 > len || self.entries.len() >= self.max_entries {
            let mut n = 0usize;
            let keep = self.entries.len() / 2;
            self.entries.retain(|_, _| {
                n += 1;
                n <= keep
            });
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    pub fn stats(&self) -> Stats {
        self.stats
    }
    pub fn reset_stats(&mut self) {
        self.stats = Stats::default();
    }
}

/// What a laid-out galley depends on besides its own text: when this changes (scale change,
/// font change, atlas rebuilt), every cached galley is stale.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FontEpoch {
    pixels_per_point: f32,
    image_size: [usize; 2],
    faces: usize,
}

impl FontEpoch {
    fn read(ctx: &egui::Context) -> Self {
        let pixels_per_point = ctx.pixels_per_point();
        ctx.fonts(|f| FontEpoch { pixels_per_point, image_size: f.font_image_size(), faces: f.definitions().font_data.len() })
    }

    /// The atlas only grows between rebuilds; a smaller one is a new atlas.
    fn still_valid(self, now: FontEpoch) -> bool {
        self.pixels_per_point == now.pixels_per_point
            && self.faces == now.faces
            && now.image_size[0] >= self.image_size[0]
            && now.image_size[1] >= self.image_size[1]
    }
}

/// Lay out `job()`, building the job outside the fonts lock (it may measure text itself).
fn layout(ctx: &egui::Context, job: impl FnOnce() -> LayoutJob) -> Arc<Galley> {
    let job = job();
    ctx.fonts_mut(|f| f.layout_job(job))
}

/// Galleys keyed by `u64` and revision. Invalidated automatically when the font atlas changes.
pub struct GalleyCache {
    cache: RevisionCache<u64, Arc<Galley>>,
    epoch: Option<FontEpoch>,
    pass: u64,
}

impl Default for GalleyCache {
    fn default() -> Self {
        Self::new(120, 8192)
    }
}

impl GalleyCache {
    pub fn new(keep_frames: u64, max_entries: usize) -> Self {
        Self { cache: RevisionCache::new(keep_frames, max_entries), epoch: None, pass: u64::MAX }
    }

    fn begin(&mut self, ctx: &egui::Context) {
        let pass = ctx.cumulative_pass_nr();
        if pass == self.pass {
            return;
        }
        if self.pass != u64::MAX {
            self.cache.end_frame();
        }
        self.pass = pass;
        let now = FontEpoch::read(ctx);
        if !self.epoch.is_some_and(|e| e.still_valid(now)) {
            self.cache.clear();
        }
        self.epoch = Some(now);
    }

    /// The galley for `key` at `revision`, laying out `job()` only when missing or stale.
    pub fn galley(&mut self, ctx: &egui::Context, key: u64, revision: u64, job: impl FnOnce() -> LayoutJob) -> Arc<Galley> {
        if let Some(g) = self.lookup(ctx, key, revision) {
            return g;
        }
        let galley = layout(ctx, job);
        self.insert(key, revision, Arc::clone(&galley));
        galley
    }

    /// The cached galley, if fresh (counts a hit).
    pub fn lookup(&mut self, ctx: &egui::Context, key: u64, revision: u64) -> Option<Arc<Galley>> {
        self.begin(ctx);
        self.cache.get(&key, revision).map(Arc::clone)
    }

    /// Store a galley laid out by the caller (counts a miss).
    pub fn insert(&mut self, key: u64, revision: u64, galley: Arc<Galley>) {
        self.cache.get_or_insert_with(key, revision, || galley);
    }

    pub fn stats(&self) -> Stats {
        self.cache.stats()
    }
    pub fn len(&self) -> usize {
        self.cache.len()
    }
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }
}

/// Meshes keyed by `u64` and revision, shared as `Arc` so painting one adds no copy.
///
/// Meshes are in screen coordinates: include the paint rect in the revision (see [`hash_of`]) or
/// build them for a fixed origin.
pub struct MeshCache {
    cache: RevisionCache<u64, Arc<Mesh>>,
}

impl Default for MeshCache {
    fn default() -> Self {
        Self { cache: RevisionCache::new(120, 4096) }
    }
}

impl MeshCache {
    pub fn new(keep_frames: u64, max_entries: usize) -> Self {
        Self { cache: RevisionCache::new(keep_frames, max_entries) }
    }
    /// The mesh for `key` at `revision`, building it only when missing or stale.
    pub fn mesh(&mut self, key: u64, revision: u64, make: impl FnOnce() -> Mesh) -> Arc<Mesh> {
        Arc::clone(self.cache.get_or_insert_with(key, revision, || Arc::new(make())))
    }
    /// Paint the cached mesh (no copy of its vertices).
    pub fn paint(&mut self, painter: &egui::Painter, key: u64, revision: u64, make: impl FnOnce() -> Mesh) {
        painter.add(egui::Shape::Mesh(self.mesh(key, revision, make)));
    }
    pub fn end_frame(&mut self) {
        self.cache.end_frame();
    }
    pub fn stats(&self) -> Stats {
        self.cache.stats()
    }
}

#[derive(Clone, Default)]
struct Shared(Arc<egui::mutex::Mutex<GalleyCache>>);

fn shared_id() -> egui::Id {
    static ID: std::sync::OnceLock<egui::Id> = std::sync::OnceLock::new();
    *ID.get_or_init(|| egui::Id::new("ashlar_ui::cache::galleys"))
}

/// The context-wide cache: a read lock on the context (the write lock only the first time).
fn shared(ctx: &egui::Context) -> Shared {
    let id = shared_id();
    match ctx.data(|d| d.get_temp::<Shared>(id)) {
        Some(s) => s,
        None => ctx.data_mut(|d| d.get_temp_mut_or_default::<Shared>(id).clone()),
    }
}

/// A galley from the context-wide cache, keyed by anything hashable plus a revision.
/// The ashlar components use this for their labels and values.
pub fn galley(ui: &egui::Ui, key: impl Hash, revision: u64, job: impl FnOnce() -> LayoutJob) -> Arc<Galley> {
    galley_in(ui.ctx(), hash_of(&key), revision, job)
}

/// [`galley`] with a precomputed key.
pub fn galley_in(ctx: &egui::Context, key: u64, revision: u64, job: impl FnOnce() -> LayoutJob) -> Arc<Galley> {
    let cache = shared(ctx);
    if let Some(g) = cache.0.lock().lookup(ctx, key, revision) {
        return g;
    }
    // no lock held while laying out: `job` may use this cache or the fonts itself
    let galley = layout(ctx, job);
    cache.0.lock().insert(key, revision, Arc::clone(&galley));
    galley
}

/// Hit/miss counts of the context-wide galley cache.
pub fn galley_stats(ctx: &egui::Context) -> Stats {
    shared(ctx).0.lock().stats()
}

/// `ui.label(text)` with its galley kept in the context-wide cache under `key` (say a row's item
/// id), so a label drawn every frame with the same text lays out nothing and allocates no
/// `String`. Draws exactly what `ui.label` draws (same font, wrapping, alignment, colors).
pub fn label(ui: &mut egui::Ui, key: impl Hash, text: &str) -> egui::Response {
    cached_label(ui, key, text, None)
}

/// [`label`] for the text of a clickable row (list, tree, menu): the text is not selectable, so a
/// click on it goes to the row instead of starting a text selection, and the label skips the
/// text-selection bookkeeping (most of a label's per-frame allocations).
pub fn row_label(ui: &mut egui::Ui, key: impl Hash, text: &str) -> egui::Response {
    cached_label(ui, key, text, Some(false))
}

fn cached_label(ui: &mut egui::Ui, key: impl Hash, text: &str, selectable: Option<bool>) -> egui::Response {
    use egui::{FontSelection, TextWrapMode};
    let with_selectable = |l: egui::Label| match selectable {
        Some(s) => l.selectable(s),
        None => l,
    };
    let layout = *ui.layout();
    let width = ui.available_width();
    let wrap = ui.wrap_mode();
    // a wrapping horizontal layout starts the text after the previous widget: egui's own layout
    if wrap == TextWrapMode::Wrap && layout.main_dir() == egui::Direction::LeftToRight && layout.main_wrap() && width.is_finite() {
        return ui.add(with_selectable(egui::Label::new(text)));
    }
    let font = FontSelection::Default.resolve(ui.style());
    let valign = ui.text_valign();
    let (halign, justify) = (layout.horizontal_placement(), layout.horizontal_justify());
    let wrap_code = match wrap {
        TextWrapMode::Extend => 0u8,
        TextWrapMode::Wrap => 1,
        TextWrapMode::Truncate => 2,
    };
    let revision = hash_of(&(text, &font, valign, halign, justify, wrap_code, width.to_bits()));
    let style = Arc::clone(ui.style());
    // the job `egui::Label` builds for plain text (a grid's left-to-right cells give the same
    // alignment it forces: left, not justified)
    let galley = galley(ui, ("ashlar_ui::cache::label", key), revision, || {
        let mut job = Arc::unwrap_or_clone(egui::WidgetText::from(text).into_layout_job(&style, FontSelection::Default, valign));
        match wrap {
            TextWrapMode::Extend => job.wrap.max_width = f32::INFINITY,
            TextWrapMode::Wrap => job.wrap.max_width = width,
            TextWrapMode::Truncate => {
                job.wrap.max_width = width;
                job.wrap.max_rows = 1;
                job.wrap.break_anywhere = true;
            }
        }
        job.halign = halign;
        job.justify = justify;
        job
    });
    let intrinsic = galley.intrinsic_size();
    let mut response = ui.add(with_selectable(egui::Label::new(galley)));
    response.set_intrinsic_size(intrinsic);
    response
}

/// The text for a button (`ui.button`, `ui.menu_button`) as a cached galley under `key`, laid
/// out exactly as the button would lay out `text`, so a button drawn every frame allocates no
/// `String` and lays out nothing. Where the button could wrap or shrink its text (the layout or
/// style wraps), the button lays it out itself, as before.
pub fn button_text(ui: &egui::Ui, key: impl Hash, text: &str) -> egui::WidgetText {
    use egui::{FontSelection, TextStyle, TextWrapMode};
    if ui.wrap_mode() != TextWrapMode::Extend {
        return text.into();
    }
    let style = ui.style();
    let font = FontSelection::default().resolve_with_fallback(style, TextStyle::Button.into());
    let color = style.visuals.override_text_color;
    let valign = ui.text_valign();
    let revision = hash_of(&(text, &font, color, valign, style.spacing.extra_text_line_spacing.to_bits()));
    egui::WidgetText::Galley(galley(ui, ("ashlar_ui::cache::button_text", key), revision, || {
        // what `WidgetText::into_galley` builds for plain text that does not wrap
        let line_height = ui.fonts_mut(|f| f.row_height(&font)) + style.spacing.extra_text_line_spacing;
        let mut job = LayoutJob::simple_format(
            text.to_owned(),
            egui::TextFormat {
                font_id: font.clone(),
                color: color.unwrap_or(egui::Color32::PLACEHOLDER),
                valign,
                line_height: Some(line_height),
                ..Default::default()
            },
        );
        job.wrap = egui::text::TextWrapping::no_max_width();
        job
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::run;

    #[test]
    fn revision_cache_rebuilds_only_on_revision_change_and_evicts_idle_entries() {
        let mut c: RevisionCache<u32, String> = RevisionCache::new(4, 100);
        let mut builds = 0;
        for _ in 0..3 {
            c.get_or_insert_with(1, 10, || {
                builds += 1;
                "a".into()
            });
            c.end_frame();
        }
        assert_eq!(builds, 1);
        c.get_or_insert_with(1, 11, || {
            builds += 1;
            "b".into()
        });
        assert_eq!(builds, 2);
        assert_eq!(c.get(&1, 11).map(String::as_str), Some("b"));
        assert_eq!(c.get(&1, 10), None);
        c.get_or_insert_with(2, 0, || "idle".into());
        for _ in 0..16 {
            c.get_or_insert_with(1, 11, unreachable_build);
            c.end_frame();
        }
        assert_eq!(c.len(), 1, "key 2 was idle and got evicted");
        assert_eq!(c.stats().misses, 3);
    }

    fn unreachable_build() -> String {
        String::from("rebuilt")
    }

    #[test]
    fn revision_cache_stays_bounded() {
        let mut c: RevisionCache<u32, u32> = RevisionCache::new(1000, 8);
        for i in 0..100 {
            c.get_or_insert_with(i, 0, || i);
        }
        assert!(c.len() <= 8);
    }

    #[test]
    fn a_full_cache_drops_old_entries_and_keeps_the_ones_in_use() {
        // 60 entries used every frame, 5 new ones a frame: the cache (100) fills with old new ones
        let mut c: RevisionCache<u32, u32> = RevisionCache::new(1000, 100);
        let mut fresh = 1000;
        let mut rebuilt_steady = 0;
        for frame in 0..50 {
            for i in 0..60 {
                if i % 12 == 0 {
                    fresh += 1;
                    c.get_or_insert_with(fresh, 0, || fresh);
                }
                c.get_or_insert_with(i, 0, || {
                    if frame > 0 {
                        rebuilt_steady += 1;
                    }
                    i
                });
            }
            assert!(c.len() <= 100);
            c.end_frame();
        }
        assert_eq!(rebuilt_steady, 0, "entries in use survive every overflow");
    }

    #[test]
    fn hash_of_is_stable() {
        assert_eq!(hash_of(&("label", 7u32)), hash_of(&("label", 7u32)));
        assert_ne!(hash_of(&("label", 7u32)), hash_of(&("label", 8u32)));
    }

    #[test]
    fn galleys_are_reused_across_frames_and_rebuilt_on_scale_change() {
        let ctx = egui::Context::default();
        let job = || LayoutJob::simple_singleline("cached label".into(), egui::FontId::proportional(13.0), egui::Color32::WHITE);
        let mut first = None;
        for _ in 0..3 {
            run(&ctx, egui::RawInput::default(), |ui| {
                let g = galley(ui, "label", 1, job);
                if let Some(prev) = &first {
                    assert!(Arc::ptr_eq(prev, &g));
                }
                first = Some(g);
            });
        }
        let s = galley_stats(&ctx);
        assert_eq!((s.hits, s.misses), (2, 1));
        ctx.set_pixels_per_point(2.0);
        run(&ctx, egui::RawInput::default(), |_| {});
        run(&ctx, egui::RawInput::default(), |ui| {
            let g = galley(ui, "label", 1, job);
            assert!(!Arc::ptr_eq(first.as_ref().unwrap(), &g), "relaid out for the new scale");
            assert_eq!(g.pixels_per_point, 2.0);
        });
    }

    #[test]
    fn a_job_may_measure_text_and_use_the_cache_itself() {
        let ctx = egui::Context::default();
        run(&ctx, egui::RawInput::default(), |ui| {
            let outer = galley(ui, "outer", 1, || {
                let inner =
                    galley(ui, "inner", 1, || LayoutJob::simple_singleline("in".into(), egui::FontId::proportional(13.0), egui::Color32::WHITE));
                let w = ui.ctx().fonts_mut(|f| f.glyph_width(&egui::FontId::proportional(13.0), 'x'));
                LayoutJob::simple_singleline(format!("{} {w}", inner.text()), egui::FontId::proportional(13.0), egui::Color32::WHITE)
            });
            assert!(outer.text().starts_with("in "));
        });
    }

    #[test]
    fn mesh_cache_shares_the_mesh() {
        let mut m = MeshCache::default();
        let a = m.mesh(1, 1, Mesh::default);
        let b = m.mesh(1, 1, panic_free_mesh);
        assert!(Arc::ptr_eq(&a, &b));
        let c = m.mesh(1, 2, Mesh::default);
        assert!(!Arc::ptr_eq(&a, &c));
    }

    fn panic_free_mesh() -> Mesh {
        let mut mesh = Mesh::default();
        mesh.add_colored_rect(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(1.0)), egui::Color32::RED);
        mesh
    }

    /// What a frame painted as text: position, text, size, rows, color, per text shape; plus the
    /// widget rects `draw` returned.
    fn painted(draw: &mut dyn FnMut(&mut egui::Ui) -> Vec<egui::Rect>) -> (Vec<String>, Vec<egui::Rect>) {
        let ctx = egui::Context::default();
        let input =
            || egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0))), ..Default::default() };
        let mut rects = Vec::new();
        // the second frame draws from the cache
        let mut out = run(&ctx, input(), |ui| rects = draw(ui));
        for _ in 0..2 {
            out = run(&ctx, input(), |ui| rects = draw(ui));
        }
        let texts = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => Some(format!(
                    "{:?} {:?} {:?} rows {} {:?} {:?}",
                    t.pos,
                    t.galley.text(),
                    t.galley.size(),
                    t.galley.rows.len(),
                    t.fallback_color,
                    t.galley.elided
                )),
                _ => None,
            })
            .collect();
        (texts, rects)
    }

    #[test]
    fn cached_labels_draw_what_egui_labels_draw() {
        let long = "a title long enough to wrap in a narrow column of a list, twice over";
        let left = egui::Layout::top_down(egui::Align::Min);
        let wrap = |w: egui::TextWrapMode| move |s: &mut egui::Style| s.wrap_mode = Some(w);
        let setups: [(&str, egui::Layout, Option<egui::TextWrapMode>); 6] = [
            ("vertical", left, None),
            ("centered", egui::Layout::top_down(egui::Align::Center), None),
            ("justified", egui::Layout::top_down_justified(egui::Align::Center), None),
            ("wrapping row", egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true), None),
            ("truncate", left, Some(egui::TextWrapMode::Truncate)),
            ("extend", left, Some(egui::TextWrapMode::Extend)),
        ];
        for (name, layout, mode) in setups {
            for horizontal in [false, true] {
                let draw = |cached: bool| {
                    move |ui: &mut egui::Ui| {
                        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(120.0, 200.0));
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(layout));
                        if let Some(m) = mode {
                            wrap(m)(child.style_mut());
                        }
                        let body = |ui: &mut egui::Ui| {
                            let mut rects = Vec::new();
                            for (i, text) in ["Note 4", "", long].into_iter().enumerate() {
                                let r = if cached { label(ui, i, text) } else { ui.label(text) };
                                rects.push(r.rect);
                            }
                            rects
                        };
                        if horizontal { child.horizontal(|ui| body(ui)).inner } else { body(&mut child) }
                    }
                };
                let (want, want_rects) = painted(&mut draw(false));
                let (got, got_rects) = painted(&mut draw(true));
                assert_eq!(got, want, "{name}, horizontal {horizontal}");
                assert_eq!(got_rects, want_rects, "{name}, horizontal {horizontal}");
            }
        }
        // the grid's own alignment
        let grid = |cached: bool| {
            move |ui: &mut egui::Ui| {
                egui::Grid::new("g")
                    .show(ui, |ui| {
                        let a = if cached { label(ui, "a", "Name") } else { ui.label("Name") };
                        let b = if cached { label(ui, "b", long) } else { ui.label(long) };
                        vec![a.rect, b.rect]
                    })
                    .inner
            }
        };
        assert_eq!(painted(&mut grid(true)), painted(&mut grid(false)), "grid");
    }

    #[test]
    fn row_labels_leave_clicks_to_the_row() {
        let ctx = egui::Context::default();
        run(&ctx, egui::RawInput::default(), |ui| {
            assert!(label(ui, 1, "Note").sense.senses_click(), "a label's text is selectable, as egui's");
            assert!(!row_label(ui, 2, "Note").sense.senses_click(), "a row label's is not");
        });
    }

    #[test]
    fn cached_button_text_draws_what_the_button_draws() {
        type Setup = fn(&mut egui::Ui);
        let setups: [(&str, Setup); 4] = [
            ("menu bar", |_| {}),
            ("override color", |ui| ui.visuals_mut().override_text_color = Some(egui::Color32::RED)),
            ("wrapping", |ui| ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap)),
            ("strong", |ui| ui.style_mut().override_text_style = Some(egui::TextStyle::Heading)),
        ];
        for (name, setup) in setups {
            let draw = |cached: bool| {
                move |ui: &mut egui::Ui| {
                    egui::MenuBar::new()
                        .ui(ui, |ui| {
                            setup(ui);
                            let mut rects = Vec::new();
                            for (i, text) in ["File", "Edit", "A longer menu title"].into_iter().enumerate() {
                                let r = if cached { ui.button(button_text(ui, i, text)) } else { ui.button(text) };
                                rects.push(r.rect);
                            }
                            rects
                        })
                        .inner
                }
            };
            assert_eq!(painted(&mut draw(true)), painted(&mut draw(false)), "{name}");
        }
    }
}
