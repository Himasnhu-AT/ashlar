//! Which records a logger keeps, in the `env_logger` directive syntax:
//! `warn,my_crate=debug,my_crate::io=trace,wgpu=off`.
//!
//! A directive is `level` (the default for every target), `target=level`, or a bare `target`
//! (everything from it). A target matches records whose target starts with it; the longest
//! matching directive wins, and a later directive for the same target replaces an earlier one.
//! Level names are any case; `off` silences. A spec of only target directives keeps nothing else
//! (`env_logger`'s rule); an empty spec is `info`. An `env_logger` message regex (`spec/regex`) is
//! ignored. Unparsable directives are skipped, so a typo never stops an app from starting, and the
//! input is capped ([`MAX_SPEC`], [`MAX_DIRECTIVES`], [`MAX_TARGET`]).

use log::{Level, LevelFilter};

/// Longest spec read, in bytes (the rest is ignored).
pub const MAX_SPEC: usize = 4096;
/// Most target directives kept (later ones are ignored).
pub const MAX_DIRECTIVES: usize = 64;
/// Longest target name, in bytes (a directive with a longer one is skipped).
pub const MAX_TARGET: usize = 128;

/// A per-target level filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    default: LevelFilter,
    /// Longest target first.
    targets: Vec<(String, LevelFilter)>,
}

impl Default for Filter {
    /// `info` for everything.
    fn default() -> Filter {
        Filter::new(LevelFilter::Info)
    }
}

impl Filter {
    /// `level` for every target.
    pub fn new(level: LevelFilter) -> Filter {
        Filter { default: level, targets: Vec::new() }
    }

    /// Parse directives (see the module docs). Never fails.
    pub fn parse(directives: &str) -> Filter {
        let mut spec = directives;
        if spec.len() > MAX_SPEC {
            let mut cut = MAX_SPEC;
            while !spec.is_char_boundary(cut) {
                cut -= 1;
            }
            spec = spec.get(..cut).unwrap_or_default();
        }
        let spec = spec.split('/').next().unwrap_or_default();
        let mut default = None;
        let mut f = Filter::new(LevelFilter::Off);
        for d in spec.split(',').map(str::trim).filter(|d| !d.is_empty()) {
            match d.split_once('=') {
                Some((t, l)) => {
                    if let Ok(l) = l.trim().parse() {
                        f.set(t.trim(), l);
                    }
                }
                None => match d.parse() {
                    Ok(l) => default = Some(l),
                    Err(_) => f.set(d, LevelFilter::Trace),
                },
            }
        }
        f.default = default.unwrap_or(if f.targets.is_empty() { LevelFilter::Info } else { LevelFilter::Off });
        f
    }

    /// Records from targets starting with `prefix` at `level` (builder form).
    pub fn target(mut self, prefix: &str, level: LevelFilter) -> Filter {
        self.set(prefix, level);
        self
    }

    fn set(&mut self, target: &str, level: LevelFilter) {
        if target.is_empty() || target.len() > MAX_TARGET {
            return;
        }
        if let Some(slot) = self.targets.iter_mut().find(|(t, _)| t == target) {
            slot.1 = level;
            return;
        }
        if self.targets.len() >= MAX_DIRECTIVES {
            return;
        }
        let at = self.targets.iter().position(|(t, _)| t.len() < target.len()).unwrap_or(self.targets.len());
        self.targets.insert(at, (target.to_string(), level));
    }

    /// Whether a record from `target` at `level` is kept.
    pub fn enabled(&self, target: &str, level: Level) -> bool {
        level <= self.level_for(target)
    }

    /// The level `target` is kept at (its longest matching directive, else the default).
    pub fn level_for(&self, target: &str) -> LevelFilter {
        self.targets.iter().find(|(t, _)| target.starts_with(t.as_str())).map_or(self.default, |(_, l)| *l)
    }

    /// The level for targets no directive names.
    pub fn default_level(&self) -> LevelFilter {
        self.default
    }

    /// The most verbose level any target keeps (for `log::set_max_level`).
    pub fn max_level(&self) -> LevelFilter {
        self.targets.iter().map(|(_, l)| *l).fold(self.default, Ord::max)
    }
}

/// The filter from the first of `vars` that is set and not blank, else from `default`
/// (e.g. `filter_from_env(&["MYAPP_LOG", "RUST_LOG"], "warn,my_app=info")`).
pub fn filter_from_env(vars: &[&str], default: &str) -> Filter {
    let spec = vars.iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()));
    Filter::parse(spec.as_deref().unwrap_or(default))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directives_match_the_longest_prefix() {
        let f = Filter::parse("warn,app=info,app_core::autosave=trace,wgpu=OFF");
        assert!(f.enabled("app_ui", Level::Info) && !f.enabled("app_ui", Level::Debug));
        assert!(f.enabled("app_core::autosave", Level::Trace));
        assert!(f.enabled("app_core::autosave::disk", Level::Trace));
        assert!(!f.enabled("app_core::commands", Level::Debug));
        assert!(!f.enabled("wgpu_core::device", Level::Error));
        assert!(f.enabled("naga", Level::Warn) && !f.enabled("naga", Level::Info));
        assert_eq!(f.max_level(), LevelFilter::Trace);
        assert_eq!(f.default_level(), LevelFilter::Warn);
    }

    #[test]
    fn plain_level_bare_target_and_typos() {
        let f = Filter::parse("debug");
        assert!(f.enabled("anything", Level::Debug) && !f.enabled("anything", Level::Trace));
        assert_eq!(f.max_level(), LevelFilter::Debug);
        let f = Filter::parse("my_cli");
        assert!(f.enabled("my_cli::x", Level::Trace) && !f.enabled("wgpu", Level::Error));
        let f = Filter::parse("app=loud, ,error");
        assert!(f.enabled("app", Level::Error) && !f.enabled("app", Level::Warn));
        assert_eq!(Filter::parse(""), Filter::parse("info"));
        assert_eq!(Filter::parse(" , ,"), Filter::default());
    }

    #[test]
    fn later_directives_replace_earlier_ones_and_regex_is_ignored() {
        let f = Filter::parse("a=debug,a=error,info/some.*regex,b=trace");
        assert!(f.enabled("a", Level::Error) && !f.enabled("a", Level::Warn));
        assert!(!f.enabled("b", Level::Trace), "everything after `/` is a message regex");
        assert_eq!(f.default_level(), LevelFilter::Info);
    }

    #[test]
    fn hostile_input_is_capped() {
        // huge, many directives, overlong targets, odd characters, multibyte at the cut
        let many: String = (0..10_000).map(|i| format!("t{i}=debug,")).collect();
        let f = Filter::parse(&many);
        assert!(f.targets.len() <= MAX_DIRECTIVES);
        let long = format!("{}=trace", "x".repeat(MAX_TARGET + 1));
        assert_eq!(Filter::parse(&long), Filter::parse("info"));
        let f = Filter::parse("=debug,==,=,debug=,\u{0}=warn,é=trace");
        assert!(f.enabled("é::x", Level::Trace));
        assert!(f.enabled("\u{0}", Level::Warn));
        let multibyte = "é".repeat(MAX_SPEC);
        let _ = Filter::parse(&multibyte);
        let _ = Filter::parse(&"=".repeat(100_000));
        let _ = Filter::parse(&",".repeat(100_000));
    }

    #[test]
    fn builder_and_env() {
        let f = Filter::new(LevelFilter::Warn).target("a", LevelFilter::Debug).target("a::b", LevelFilter::Off);
        assert!(f.enabled("a::c", Level::Debug) && !f.enabled("a::b", Level::Error));
        // vars that are not set fall through to the default
        let f = filter_from_env(&["ASHLAR_LOG_TEST_UNSET_1", "ASHLAR_LOG_TEST_UNSET_2"], "error");
        assert_eq!(f, Filter::new(LevelFilter::Error));
    }
}
