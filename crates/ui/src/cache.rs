//! Revision-keyed caching of galleys, meshes and other derived paint data across frames.
//!
//! A cached value is identified by a key (what it is: a row id, a label's widget id) and is valid
//! while its revision (what it was built from: a document revision, a value's bits) is
//! unchanged. Nothing is diffed: the caller says when the input changed by changing the revision.
//! Entries unused for a number of frames are evicted.
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
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use egui::{Galley, Mesh, text::LayoutJob};

/// Hash anything into a revision or key value.
pub fn hash_of(value: &impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut h);
    h.finish()
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
    entries: HashMap<K, Entry<V>>,
    frame: u64,
    keep_frames: u64,
    max_entries: usize,
    stats: Stats,
}

impl<K: Eq + Hash, V> RevisionCache<K, V> {
    /// Keep entries for `keep_frames` frames after their last use, and at most `max_entries`.
    pub fn new(keep_frames: u64, max_entries: usize) -> Self {
        Self { entries: HashMap::new(), frame: 0, keep_frames, max_entries: max_entries.max(1), stats: Stats::default() }
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
        if !self.entries.contains_key(&key) && self.entries.len() >= self.max_entries {
            self.evict_older_than(frame);
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
        // still full (every entry used this frame): drop everything rather than grow
        if self.entries.len() >= self.max_entries {
            self.entries.clear();
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
        self.begin(ctx);
        Arc::clone(self.cache.get_or_insert_with(key, revision, || ctx.fonts_mut(|f| f.layout_job(job()))))
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

fn shared(ctx: &egui::Context) -> Shared {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Shared>(egui::Id::new("ashlar_ui::cache::galleys")).clone())
}

/// A galley from the context-wide cache, keyed by anything hashable plus a revision.
/// The ashlar components use this for their labels and values.
pub fn galley(ui: &egui::Ui, key: impl Hash, revision: u64, job: impl FnOnce() -> LayoutJob) -> Arc<Galley> {
    galley_in(ui.ctx(), hash_of(&key), revision, job)
}

/// [`galley`] with a precomputed key.
pub fn galley_in(ctx: &egui::Context, key: u64, revision: u64, job: impl FnOnce() -> LayoutJob) -> Arc<Galley> {
    let cache = shared(ctx);
    let mut guard = cache.0.lock();
    guard.galley(ctx, key, revision, job)
}

/// Hit/miss counts of the context-wide galley cache.
pub fn galley_stats(ctx: &egui::Context) -> Stats {
    shared(ctx).0.lock().stats()
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
}
