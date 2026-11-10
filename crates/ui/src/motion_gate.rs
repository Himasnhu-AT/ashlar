//! A motion and jank gate for tests: record what moved, then flag motion that looks wrong.
//!
//! ```rust,ignore
//! use ashlar_ui::motion_gate::{MotionRecorder, MotionRules};
//! let rec = MotionRecorder::install(&ctx);
//! // run frames on a virtual clock, calling `rec.frame(&ctx, &output)` after each one
//! let report = rec.check(&MotionRules::default());
//! assert!(report.is_clean(), "{report}");
//! ```
//!
//! Two kinds of record go in:
//! - **Values**: every value sampled through [`crate::motion`] while a recorder is installed
//!   ([`Sample`]: id, time, value, velocity, target, class, spring bounce). Without a recorder the
//!   motion code pays one `Option` check per call.
//! - **Frames**: [`MotionRecorder::frame`] notes the frame's time, how soon it asked for the next
//!   frame, the wake reasons requested during it, and the rect of every automation element.
//!
//! [`MotionRecorder::check`] then reports ([`Rule`]):
//! - **Overshoot**: an opacity or colour value passes its target (effects never bounce).
//! - **Reversal**: a value with no bounce turns back (moves away from its target after moving
//!   toward it, or crosses it).
//! - **Pop**: a moving value lands far from where its velocity said it would be, or a moving
//!   element's rect jumps between frames.
//! - **Flicker**: an opacity goes shown–hidden–shown (or back) within a short window, or an
//!   element vanishes for a frame or two and comes back.
//! - **Idle**: frames were still asked for after everything settled.
//!
//! Each rule reports at most once per value or element (with how often it happened). An
//! [`Allow`] in [`MotionRules::allow`] moves matching findings to [`MotionReport::allowed`], with
//! its reason.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ashlar_motion::{Class, Lanes};
use egui::Id;

use crate::wake::Scheduler;

/// Records kept at most (beyond this they are counted as dropped).
pub const MAX_RECORDS: usize = 1_000_000;

/// One sample of an animated value, taken as [`crate::motion`] computed it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// The value's id.
    pub id: Id,
    /// Motion time in seconds (velocities are per second of this time).
    pub t: f64,
    /// The egui time of the frame.
    pub time: f64,
    /// Lanes used by the value (1 for `f32`, 4 for a rect or colour).
    pub lanes: usize,
    pub value: Lanes,
    pub velocity: Lanes,
    pub target: Lanes,
    pub class: Class,
    /// The spring's bounce (0 for tweens and other curves).
    pub bounce: f32,
    /// Set by the hand ([`crate::motion::set`]): it follows input, so the rules leave it alone.
    pub held: bool,
}

impl Sample {
    /// A sample for seeding the gate by hand (one lane, velocity 0, not held).
    pub fn new(id: Id, t: f64, value: f32, target: f32, class: Class) -> Self {
        Sample {
            id,
            t,
            time: t,
            lanes: 1,
            value: [value, 0.0, 0.0, 0.0],
            velocity: [0.0; 4],
            target: [target, 0.0, 0.0, 0.0],
            class,
            bounce: 0.0,
            held: false,
        }
    }

    /// With this velocity in lane 0.
    pub fn velocity(mut self, v: f32) -> Self {
        self.velocity[0] = v;
        self
    }

    /// With this spring bounce.
    pub fn bounce(mut self, bounce: f32) -> Self {
        self.bounce = bounce;
        self
    }
}

/// One recorded frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    /// egui time of the frame.
    pub time: f64,
    /// How soon the frame asked for the next one, in seconds (`None`: it asked for none).
    pub repaint_after: Option<f64>,
    /// Wake reasons requested during the frame.
    pub reasons: Vec<String>,
    /// Automation elements: id and `[x, y, width, height]`.
    pub rects: Vec<(String, [f32; 4])>,
}

#[derive(Default)]
struct Recording {
    samples: Vec<Sample>,
    frames: Vec<Frame>,
    names: BTreeMap<u64, String>,
    dropped: usize,
    /// Wake requests per reason at the last frame (to tell which were new).
    requests: BTreeMap<String, u64>,
}

/// Where [`crate::motion`] sends samples while a recorder is installed.
#[derive(Clone, Default)]
pub(crate) struct Sink(Arc<Mutex<Recording>>);

impl Sink {
    fn lock(&self) -> MutexGuard<'_, Recording> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn push(&self, s: Sample) {
        let mut r = self.lock();
        if r.samples.len() < MAX_RECORDS {
            r.samples.push(s);
        } else {
            r.dropped = r.dropped.saturating_add(1);
        }
    }
}

/// Records motion samples and frames for [`MotionRecorder::check`]. Cheap to clone (a shared
/// handle).
#[derive(Clone, Default)]
pub struct MotionRecorder(Sink);

impl MotionRecorder {
    /// A recorder that is not installed anywhere (seed it with [`Self::push`] and
    /// [`Self::push_frame`]).
    pub fn new() -> Self {
        Self::default()
    }

    /// Start recording the values `ctx` animates (replacing any recorder installed before).
    pub fn install(ctx: &egui::Context) -> Self {
        let rec = Self::new();
        crate::motion::set_recorder(ctx, Some(rec.0.clone()));
        rec.baseline(ctx);
        rec
    }

    /// Stop recording `ctx`'s values (whichever recorder is installed).
    pub fn uninstall(ctx: &egui::Context) {
        crate::motion::set_recorder(ctx, None);
    }

    fn baseline(&self, ctx: &egui::Context) {
        let stats = Scheduler::of(ctx).stats();
        self.0.lock().requests = stats.requests_by;
    }

    /// Name a value's id in reports (ids are hashes otherwise).
    pub fn name(&self, id: Id, name: impl Into<String>) {
        self.0.lock().names.insert(id.value(), name.into());
    }

    /// Add a sample by hand.
    pub fn push(&self, s: Sample) {
        self.0.push(s);
    }

    /// Add a frame by hand.
    pub fn push_frame(&self, f: Frame) {
        let mut r = self.0.lock();
        if r.frames.len() < MAX_RECORDS {
            r.frames.push(f);
        } else {
            r.dropped = r.dropped.saturating_add(1);
        }
    }

    /// Note the frame `ctx` just ran, which gave `out`: its time, when it asked for the next
    /// frame, the wake reasons requested since the last frame, and the automation rects.
    pub fn frame(&self, ctx: &egui::Context, out: &egui::FullOutput) {
        let delay = out.viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.repaint_delay);
        let repaint_after = delay.filter(|d| *d < std::time::Duration::MAX).map(|d| d.as_secs_f64());
        let time = ctx.input(|i| i.time);
        let stats = Scheduler::of(ctx).stats();
        let mut seen = std::collections::BTreeSet::new();
        let rects = crate::automation::inspect(ctx).elements.into_iter().filter(|e| seen.insert(e.id.clone())).map(|e| (e.id, e.rect)).collect();
        let reasons = {
            let mut r = self.0.lock();
            let reasons = stats.requests_by.iter().filter(|(k, n)| r.requests.get(*k).copied().unwrap_or(0) < **n).map(|(k, _)| k.clone()).collect();
            r.requests = stats.requests_by;
            reasons
        };
        self.push_frame(Frame { time, repaint_after, reasons, rects });
    }

    /// The samples so far.
    pub fn samples(&self) -> Vec<Sample> {
        self.0.lock().samples.clone()
    }

    /// The frames so far.
    pub fn frames(&self) -> Vec<Frame> {
        self.0.lock().frames.clone()
    }

    /// Forget everything recorded so far (names stay).
    pub fn clear(&self) {
        let mut r = self.0.lock();
        r.samples.clear();
        r.frames.clear();
        r.dropped = 0;
    }

    /// Check the recording against `rules`.
    pub fn check(&self, rules: &MotionRules) -> MotionReport {
        let r = self.0.lock();
        check(&r, rules)
    }
}

/// What a finding is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subject {
    /// An animated value (with its name, when given through [`MotionRecorder::name`]).
    Value(Id, Option<String>),
    /// An automation element.
    Element(String),
    /// The frame loop, with the wake reasons that kept it going (`repaint` when none was given).
    Frames(String),
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Subject::Value(_, Some(name)) => write!(f, "value {name}"),
            Subject::Value(id, None) => write!(f, "value {}", id.short_debug_format()),
            Subject::Element(id) => write!(f, "element {id}"),
            Subject::Frames(reasons) => write!(f, "frames ({reasons})"),
        }
    }
}

/// The kinds of finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rule {
    Overshoot,
    Reversal,
    Pop,
    Flicker,
    Idle,
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Rule::Overshoot => "overshoot",
            Rule::Reversal => "reversal",
            Rule::Pop => "pop",
            Rule::Flicker => "flicker",
            Rule::Idle => "idle",
        })
    }
}

/// One thing that looked wrong.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub rule: Rule,
    pub subject: Subject,
    /// egui time of the first occurrence.
    pub time: f64,
    /// How many times it happened.
    pub count: usize,
    pub detail: String,
    /// Why it is allowed (only for findings in [`MotionReport::allowed`]).
    pub allowed_because: Option<String>,
}

/// An exception: findings of `rule` (any rule when `None`) about a matching subject are allowed.
#[derive(Clone, Debug, PartialEq)]
pub struct Allow {
    pub matches: Match,
    pub rule: Option<Rule>,
    pub reason: String,
}

/// What an [`Allow`] matches.
#[derive(Clone, Debug, PartialEq)]
pub enum Match {
    /// A value by id.
    Value(Id),
    /// A value by the name it was given, or an element by id: exactly, or as a prefix when the
    /// pattern ends in `*`.
    Name(String),
    /// Idle frames requested for this wake reason.
    Reason(String),
}

impl Allow {
    /// Allow `rule` (or any rule) for the value `id`.
    pub fn value(id: Id, rule: Option<Rule>, reason: impl Into<String>) -> Self {
        Allow { matches: Match::Value(id), rule, reason: reason.into() }
    }

    /// Allow `rule` (or any rule) for values named, or elements with ids, matching `pattern`
    /// (a trailing `*` matches any rest).
    pub fn name(pattern: impl Into<String>, rule: Option<Rule>, reason: impl Into<String>) -> Self {
        Allow { matches: Match::Name(pattern.into()), rule, reason: reason.into() }
    }

    /// Allow frames after settle that were asked for with this wake reason.
    pub fn reason(reason: impl Into<String>, why: impl Into<String>) -> Self {
        Allow { matches: Match::Reason(reason.into()), rule: Some(Rule::Idle), reason: why.into() }
    }

    fn applies(&self, f: &Finding) -> bool {
        if self.rule.is_some_and(|r| r != f.rule) {
            return false;
        }
        let pat = |p: &str, s: &str| match p.strip_suffix('*') {
            Some(prefix) => s.starts_with(prefix),
            None => p == s,
        };
        match (&self.matches, &f.subject) {
            (Match::Value(a), Subject::Value(b, _)) => a == b,
            (Match::Name(p), Subject::Value(_, Some(n)) | Subject::Element(n)) => pat(p, n),
            (Match::Reason(r), Subject::Frames(reasons)) => reasons.split(", ").all(|x| x == r),
            _ => false,
        }
    }
}

/// Thresholds for [`MotionRecorder::check`].
#[derive(Clone, Debug, PartialEq)]
pub struct MotionRules {
    /// How far past its target an opacity or colour may go (in its 0..1 units).
    pub overshoot_eps: f32,
    /// Smallest step that counts as movement, per class: spatial values in points.
    pub spatial_eps: f32,
    /// The same for other classes (opacity, colour, scale).
    pub unit_eps: f32,
    /// A moving value may land at most this many points (spatial) from where its velocity said.
    pub value_jump_pt: f32,
    /// A moving element's edges may change pace by at most this many points between frames.
    pub rect_jump_pt: f32,
    /// Below this an opacity counts as hidden.
    pub hidden_below: f32,
    /// Above this an opacity counts as shown.
    pub shown_above: f32,
    /// Two opposite opacity changes within this many seconds are a flicker.
    pub flicker_window: f64,
    /// An element missing for at most this many frames and then back is a flicker.
    pub flicker_frames: usize,
    /// A frame asking for the next one within this many seconds counts as a frame requested.
    pub idle_delay: f64,
    /// Frames that may still be requested after everything settled.
    pub idle_grace: usize,
    /// Exceptions, with reasons.
    pub allow: Vec<Allow>,
}

impl Default for MotionRules {
    fn default() -> Self {
        MotionRules {
            overshoot_eps: 0.002,
            spatial_eps: 0.05,
            unit_eps: 1e-4,
            value_jump_pt: 4.0,
            rect_jump_pt: 40.0,
            hidden_below: 0.1,
            shown_above: 0.9,
            flicker_window: 0.3,
            flicker_frames: 2,
            idle_delay: 0.1,
            idle_grace: 3,
            allow: Vec::new(),
        }
    }
}

impl MotionRules {
    /// These rules with one more exception.
    pub fn allowing(mut self, a: Allow) -> Self {
        self.allow.push(a);
        self
    }

    fn eps(&self, class: Class) -> f32 {
        if matches!(class, Class::Spatial | Class::Gesture) { self.spatial_eps } else { self.unit_eps }
    }
}

/// What [`MotionRecorder::check`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MotionReport {
    pub findings: Vec<Finding>,
    /// Findings an [`Allow`] matched.
    pub allowed: Vec<Finding>,
    pub samples: usize,
    pub frames: usize,
    /// Records dropped past [`MAX_RECORDS`].
    pub dropped: usize,
}

impl MotionReport {
    /// No findings (allowed ones aside).
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    /// Findings of `rule`.
    pub fn of(&self, rule: Rule) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(move |f| f.rule == rule)
    }
}

impl fmt::Display for MotionReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "motion gate: {} finding(s), {} allowed ({} samples, {} frames{})",
            self.findings.len(),
            self.allowed.len(),
            self.samples,
            self.frames,
            if self.dropped > 0 { format!(", {} dropped", self.dropped) } else { String::new() }
        )?;
        for x in &self.findings {
            writeln!(f, "  {:<9} {} at {:.3}s (x{}): {}", x.rule, x.subject, x.time, x.count, x.detail)?;
        }
        for x in &self.allowed {
            let why = x.allowed_because.as_deref().unwrap_or("");
            writeln!(f, "  allowed {} {} (x{}): {why}", x.rule, x.subject, x.count)?;
        }
        Ok(())
    }
}

/// Findings per (rule, subject), keeping the first occurrence and a count.
#[derive(Default)]
struct Found(BTreeMap<(Rule, String), Finding>);

impl Found {
    fn add(&mut self, rule: Rule, subject: Subject, time: f64, detail: impl FnOnce() -> String) {
        self.0.entry((rule, format!("{subject:?}"))).and_modify(|f| f.count = f.count.saturating_add(1)).or_insert_with(|| Finding {
            rule,
            subject,
            time,
            count: 1,
            detail: detail(),
            allowed_because: None,
        });
    }
}

fn check(r: &Recording, rules: &MotionRules) -> MotionReport {
    let mut found = Found::default();
    // the egui time after which nothing moved
    let mut settled_at = f64::NEG_INFINITY;

    // values: one track per id, value type and class, in time order, the last sample per time
    let mut tracks: BTreeMap<(u64, usize, u8), Vec<&Sample>> = BTreeMap::new();
    for s in &r.samples {
        let track = tracks.entry((s.id.value(), s.lanes, class_key(s.class))).or_default();
        match track.last_mut() {
            Some(last) if last.t == s.t => *last = s,
            _ => track.push(s),
        }
    }
    for track in tracks.values_mut() {
        track.sort_by(|a, b| a.t.total_cmp(&b.t));
        let Some(first) = track.first() else { continue };
        let subject = Subject::Value(first.id, r.names.get(&first.id.value()).cloned());
        settled_at = settled_at.max(check_track(track, rules, &subject, &mut found));
    }

    // elements
    settled_at = settled_at.max(check_rects(&r.frames, rules, &mut found));

    // idle: frames asked for after everything settled (including frames that moved nothing)
    let late: Vec<&Frame> = r.frames.iter().filter(|f| f.time > settled_at && f.repaint_after.is_some_and(|d| d <= rules.idle_delay)).collect();
    if late.len() > rules.idle_grace {
        let mut reasons: Vec<&str> = late.iter().flat_map(|f| f.reasons.iter().map(String::as_str)).collect();
        reasons.sort_unstable();
        reasons.dedup();
        let reasons = if reasons.is_empty() { "repaint".to_owned() } else { reasons.join(", ") };
        let first = late.first().map_or(0.0, |f| f.time);
        let n = late.len();
        let settled = if settled_at.is_finite() { format!("{settled_at:.3}s") } else { "the start".to_owned() };
        found.add(Rule::Idle, Subject::Frames(reasons), first, || {
            format!("{n} frames requested after everything settled at {settled} (grace {})", rules.idle_grace)
        });
        if let Some(f) = found.0.values_mut().find(|f| f.rule == Rule::Idle) {
            f.count = n;
        }
    }

    let mut report = MotionReport { samples: r.samples.len(), frames: r.frames.len(), dropped: r.dropped, ..MotionReport::default() };
    for (_, mut f) in found.0 {
        match rules.allow.iter().find(|a| a.applies(&f)) {
            Some(a) => {
                f.allowed_because = Some(a.reason.clone());
                report.allowed.push(f);
            }
            None => report.findings.push(f),
        }
    }
    report.findings.sort_by(|a, b| a.time.total_cmp(&b.time));
    report
}

fn class_key(c: Class) -> u8 {
    match c {
        Class::Spatial => 0,
        Class::Scale => 1,
        Class::Opacity => 2,
        Class::Color => 3,
        Class::Gesture => 4,
        Class::Other => 5,
    }
}

/// Check one value's samples; returns the egui time of its last movement.
fn check_track(track: &[&Sample], rules: &MotionRules, subject: &Subject, found: &mut Found) -> f64 {
    let mut last_moving = f64::NEG_INFINITY;
    let Some(&first) = track.first() else { return last_moving };
    let lanes = first.lanes.clamp(1, 4);
    let class = first.class;
    let eps = rules.eps(class);
    let effect = matches!(class, Class::Opacity | Class::Color);
    let mut overshot = false;
    // per lane: the side of the target the segment started on, and whether it moved toward it
    let mut side = [0.0f32; 4];
    let mut toward = [false; 4];
    // flicker: the last shown/hidden state and when it changed
    let mut shown: Option<bool> = None;
    let mut changes: Vec<(f64, bool)> = Vec::new();
    for (i, &s) in track.iter().enumerate() {
        let lanes_of = |x: &Lanes| x.iter().take(lanes).copied().collect::<Vec<f32>>();
        let unsettled = (0..lanes).any(|k| (s.value[k] - s.target[k]).abs() > eps || s.velocity[k].abs() > eps);
        if unsettled {
            last_moving = last_moving.max(s.time);
        }
        if s.held {
            // set by hand (a reset or a drag): what follows starts afresh
            shown = None;
            changes.clear();
        } else if class == Class::Opacity {
            let now = if s.value[0] >= rules.shown_above {
                Some(true)
            } else if s.value[0] <= rules.hidden_below {
                Some(false)
            } else {
                shown
            };
            if now != shown {
                if shown.is_some()
                    && let Some(&(t0, _)) = changes.last()
                    && s.time - t0 <= rules.flicker_window
                {
                    found.add(Rule::Flicker, subject.clone(), s.time, || {
                        format!("opacity went {} and back within {:.3}s", if now == Some(true) { "hidden" } else { "shown" }, s.time - t0)
                    });
                }
                if shown.is_some() {
                    changes.push((s.time, now == Some(true)));
                }
                shown = now;
            }
        }
        let Some(&prev) = i.checked_sub(1).and_then(|p| track.get(p)) else {
            for (k, sd) in side.iter_mut().enumerate().take(lanes) {
                let d = s.target[k] - s.value[k];
                *sd = if d.abs() > eps { d.signum() } else { 0.0 };
            }
            continue;
        };
        let retarget = (0..lanes).any(|k| (s.target[k] - prev.target[k]).abs() > eps);
        if retarget || s.held || prev.held {
            for k in 0..lanes {
                let d = s.target[k] - prev.value[k];
                side[k] = if d.abs() > eps { d.signum() } else { 0.0 };
                toward[k] = false;
            }
            if s.held || prev.held {
                continue;
            }
        }
        let dt = (s.t - prev.t) as f32;
        for k in 0..lanes {
            let d = s.value[k] - prev.value[k];
            let gap = s.target[k] - s.value[k];
            let crossed = side[k] != 0.0 && gap * side[k] < -eps;
            if effect && crossed && gap * side[k] < -rules.overshoot_eps {
                overshot = true;
                found.add(Rule::Overshoot, subject.clone(), s.time, || {
                    format!("lane {k} went {:.4} past its target {:.4} ({:?})", -gap * side[k], s.target[k], class)
                });
            }
            // a value without bounce only ever heads for its target
            if class != Class::Gesture && s.bounce <= 0.0 && !retarget && !(effect && overshot) {
                let heading = d * (prev.target[k] - prev.value[k]);
                let turned = d.abs() > eps && heading < 0.0 && toward[k];
                if turned || (crossed && !effect) {
                    found.add(Rule::Reversal, subject.clone(), s.time, || {
                        format!("lane {k} turned back at {:.3} (target {:.3}) with bounce 0", prev.value[k], s.target[k])
                    });
                }
                if d.abs() > eps {
                    toward[k] = heading > 0.0;
                }
            }
            if class == Class::Spatial && !retarget && dt > 0.0 {
                let moving = prev.velocity[k].abs() > eps || (prev.value[k] - prev.target[k]).abs() > eps;
                let expected = (prev.velocity[k] + s.velocity[k]) * 0.5 * dt;
                let off = d - expected;
                if moving && off.abs() > rules.value_jump_pt {
                    found.add(Rule::Pop, subject.clone(), s.time, || {
                        format!(
                            "lane {k} moved {d:.2} pt in {:.4}s where its velocity said {expected:.2} pt ({:?} -> {:?})",
                            dt,
                            lanes_of(&prev.value),
                            lanes_of(&s.value)
                        )
                    });
                }
            }
        }
    }
    last_moving
}

/// Check elements across frames; returns the egui time of the last frame where one moved.
fn check_rects(frames: &[Frame], rules: &MotionRules, found: &mut Found) -> f64 {
    let mut last_change = f64::NEG_INFINITY;
    // per element: frame index and rect of each frame it was in
    let mut seen: BTreeMap<&str, Vec<(usize, f64, [f32; 4])>> = BTreeMap::new();
    for (i, f) in frames.iter().enumerate() {
        for (id, rect) in &f.rects {
            seen.entry(id.as_str()).or_default().push((i, f.time, *rect));
        }
    }
    let edges = |[x, y, w, h]: [f32; 4]| [x, y, x + w, y + h];
    for (id, track) in &seen {
        let subject = || Subject::Element((*id).to_owned());
        // appearing or vanishing changes what is on screen
        if let (Some(first), Some(last)) = (track.first(), track.last()) {
            if first.0 > 0 {
                last_change = last_change.max(first.1);
            }
            if let Some(after) = frames.get(last.0 + 1) {
                last_change = last_change.max(after.time);
            }
        }
        let mut prev_delta: Option<[f32; 4]> = None;
        for w in 0..track.len() {
            let Some(&(i, time, rect)) = track.get(w) else { continue };
            let Some(&(pi, _, prect)) = w.checked_sub(1).and_then(|p| track.get(p)) else { continue };
            if i != pi + 1 {
                // a gap: the element was missing for a while
                let missing = i - pi - 1;
                last_change = last_change.max(time);
                if missing <= rules.flicker_frames {
                    found.add(Rule::Flicker, subject(), time, || format!("vanished for {missing} frame(s) and came back"));
                }
                prev_delta = None;
                continue;
            }
            let (a, b) = (edges(prect), edges(rect));
            let delta = [b[0] - a[0], b[1] - a[1], b[2] - a[2], b[3] - a[3]];
            let moved = delta.iter().any(|d| d.abs() > rules.spatial_eps);
            if moved {
                last_change = last_change.max(time);
            }
            let next_moves = track.get(w + 1).is_some_and(|&(ni, _, nrect)| {
                let c = edges(nrect);
                ni == i + 1 && (0..4).any(|k| (c[k] - b[k]).abs() > rules.spatial_eps)
            });
            let was_moving = prev_delta.is_some_and(|p| p.iter().any(|d| d.abs() > rules.spatial_eps));
            let pd = prev_delta.unwrap_or([0.0; 4]);
            if (was_moving || next_moves)
                && moved
                && let Some(k) = (0..4).find(|&k| (delta[k] - pd[k]).abs() > rules.rect_jump_pt)
            {
                found.add(Rule::Pop, subject(), time, || {
                    format!("edge {k} moved {:.1} pt in one frame after {:.1} pt (rect {:?} -> {:?})", delta[k], pd[k], prect, rect)
                });
            }
            prev_delta = Some(delta);
        }
    }
    last_change
}
