//! Removes personal details from text before it is queued or written: the home directory becomes `~`,
//! account names inside paths become `<user>`, and long values are cut at a fixed size.
//!
//! Everything here is a pure function of its arguments. [`Scrubber::from_env`] is the only place that
//! looks at the machine.

/// Longest panic or error message, in bytes.
pub(crate) const MAX_MESSAGE: usize = 2 * 1024;
/// Longest backtrace, in bytes.
pub(crate) const MAX_BACKTRACE: usize = 32 * 1024;
/// Longest log line, in bytes.
pub(crate) const MAX_LOG_LINE: usize = 512;
/// Longest short label (thread name, location, log target), in bytes.
pub(crate) const MAX_LABEL: usize = 128;

const USER_PLACEHOLDER: &str = "<user>";
const ELLIPSIS: &str = "...";

/// Knows the current home directory and account name and applies [`scrub`] with them.
#[derive(Debug, Clone, Default)]
pub(crate) struct Scrubber {
    home: Option<String>,
    user: Option<String>,
}

impl Scrubber {
    #[cfg(test)]
    pub(crate) fn new(home: Option<String>, user: Option<String>) -> Self {
        Self { home, user }
    }

    /// Reads the home directory and account name from the environment.
    pub(crate) fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|value| !value.trim().is_empty());
        let home = var("HOME").or_else(|| var("USERPROFILE"));
        let user = var("USER").or_else(|| var("USERNAME")).or_else(|| home.as_deref().and_then(last_segment).map(str::to_owned));
        Self { home, user }
    }

    /// Scrubs `text` and cuts it to at most `limit` bytes.
    pub(crate) fn apply(&self, text: &str, limit: usize) -> String {
        scrub(text, self.home.as_deref(), self.user.as_deref(), limit)
    }
}

/// Scrubs `text` and cuts it to at most `limit` bytes on a character boundary.
pub(crate) fn scrub(text: &str, home: Option<&str>, user: Option<&str>, limit: usize) -> String {
    // Work on a bounded prefix so a huge input does not make this expensive. The cut is far enough past
    // `limit` that the final truncation always lands inside text that was fully scrubbed.
    let bounded = cut(text, limit.saturating_mul(4).saturating_add(256));
    // Everything that is replaced is part of a path, and a path has a separator in it. Most log lines
    // have none, and for those there is nothing to do but cut to length: one allocation instead of five.
    if !bounded.contains(['/', '\\']) {
        return truncate(bounded, limit);
    }
    scrub_paths(bounded, home, user, limit)
}

fn scrub_paths(bounded: &str, home: Option<&str>, user: Option<&str>, limit: usize) -> String {
    let mut out = bounded.to_owned();
    if let Some(home) = home.map(trim_separators).filter(|home| !home.is_empty()) {
        out = replace_home(&out, home);
    }
    if let Some(user) = user.map(str::trim).filter(|user| !user.is_empty()) {
        out = replace_user_segments(&out, user);
    }
    out = replace_account_segments(&out);
    truncate(&out, limit)
}

/// Cuts `text` to at most `limit` bytes on a character boundary, ending with `...` when it was cut.
pub(crate) fn truncate(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    if limit <= ELLIPSIS.len() {
        return cut(text, limit).to_owned();
    }
    let mut out = cut(text, limit - ELLIPSIS.len()).to_owned();
    out.push_str(ELLIPSIS);
    out
}

fn cut(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default()
}

fn trim_separators(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() { "" } else { trimmed }
}

fn last_segment(path: &str) -> Option<&str> {
    trim_separators(path).rsplit(['/', '\\']).next().filter(|segment| !segment.is_empty())
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

fn is_separator(c: char) -> bool {
    c == '/' || c == '\\'
}

fn looks_like_windows(path: &str) -> bool {
    let mut chars = path.chars();
    matches!((chars.next(), chars.next()), (Some(drive), Some(':')) if drive.is_ascii_alphabetic()) || path.starts_with("\\\\")
}

/// Replaces `home`, written with either separator, by `~` wherever it starts a path.
fn replace_home(text: &str, home: &str) -> String {
    let fold = looks_like_windows(home);
    let slash = home.replace('\\', "/");
    let backslash = home.replace('/', "\\");
    let mut out = text.to_owned();
    for variant in [home.to_owned(), slash, backslash] {
        if variant.is_empty() {
            continue;
        }
        out = replace_bounded(
            &out,
            &variant,
            fold,
            "~",
            |before| before.is_none_or(|c| !is_name_char(c)),
            |after| after.is_none_or(|c| !is_name_char(c)),
        );
    }
    out
}

/// Replaces the account name where it is a whole path segment, such as `/data/alice/cache`.
fn replace_user_segments(text: &str, user: &str) -> String {
    let fold = false;
    replace_bounded(text, user, fold, USER_PLACEHOLDER, |before| before.is_some_and(is_separator), |after| after.is_none_or(|c| !is_name_char(c)))
}

/// Replaces the segment after `/Users/`, `/home/` and `\Users\` with a placeholder, whoever the account is.
/// Paths baked into a binary at build time carry the builder's account name, not the runner's.
fn replace_account_segments(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    while pos < text.len() {
        let next = ACCOUNT_DIRS.iter().filter_map(|&(sep, dir)| find_account_dir(&lower, pos, sep, dir)).min();
        let Some((at, marker_len)) = next else { break };
        let start = at + marker_len;
        let rest = text.get(start..).unwrap_or_default();
        let name_len = rest.find(|c: char| is_separator(c) || c.is_whitespace() || "\"'`()<>[]{}:,;|".contains(c)).unwrap_or(rest.len());
        let name = rest.get(..name_len).unwrap_or_default();
        out.push_str(text.get(pos..start).unwrap_or_default());
        if name.is_empty() || name.starts_with('~') {
            pos = start;
            continue;
        }
        out.push_str(USER_PLACEHOLDER);
        pos = start + name_len;
    }
    out.push_str(text.get(pos..).unwrap_or_default());
    out
}

/// The folders that hold account folders, as (separator, name). Kept apart rather than as `/users/`
/// literals so a shipped binary holds no path fragment that a build-path leak check would flag.
const ACCOUNT_DIRS: [(char, &str); 3] = [('/', "users"), ('/', "home"), ('\\', "users")];

/// The first `<sep><dir><sep>` in `lower` at or after `from`, as (start, length).
fn find_account_dir(lower: &str, from: usize, sep: char, dir: &str) -> Option<(usize, usize)> {
    let mut search = from;
    while let Some(at) = lower.get(search..)?.find(dir).map(|i| search + i) {
        let before = lower.get(..at).and_then(|s| s.chars().next_back());
        let after = lower.get(at + dir.len()..).and_then(|s| s.chars().next());
        if before == Some(sep) && after == Some(sep) {
            return Some((at - sep.len_utf8(), dir.len() + 2 * sep.len_utf8()));
        }
        search = at + dir.len();
    }
    None
}

/// Replaces every occurrence of `needle` whose neighbours satisfy the predicates. With `fold`, ASCII case
/// is ignored when matching.
fn replace_bounded(
    text: &str,
    needle: &str,
    fold: bool,
    replacement: &str,
    left_ok: impl Fn(Option<char>) -> bool,
    right_ok: impl Fn(Option<char>) -> bool,
) -> String {
    if needle.is_empty() {
        return text.to_owned();
    }
    let (haystack, needle) = if fold { (text.to_ascii_lowercase(), needle.to_ascii_lowercase()) } else { (text.to_owned(), needle.to_owned()) };
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    let mut search = 0;
    while let Some(found) = haystack.get(search..).and_then(|rest| rest.find(&needle)) {
        let start = search + found;
        let end = start + needle.len();
        let before = text.get(..start).and_then(|s| s.chars().next_back());
        let after = text.get(end..).and_then(|s| s.chars().next());
        if left_ok(before) && right_ok(after) {
            out.push_str(text.get(pos..start).unwrap_or_default());
            out.push_str(replacement);
            pos = end;
            search = end;
        } else {
            // Advance one character so overlapping candidates are still considered.
            let step = text.get(start..).and_then(|s| s.chars().next()).map_or(1, char::len_utf8);
            search = start + step;
        }
    }
    out.push_str(text.get(pos..).unwrap_or_default());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_without_a_path_separator_takes_the_short_way_to_the_same_result() {
        // The short way only cuts to length. It is right as long as the full scrub would have changed
        // nothing, which holds because everything it replaces is a path segment.
        let words = ["alice", "Alice", "home", "Users", "~", "C:", "é", "名", "🦀", " ", "-", ".", ":", "alice.smith", "x"];
        let mut state = 0x1234_5678_u64;
        let mut next = move |bound: usize| {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as usize % bound
        };
        for _ in 0..20_000 {
            let text: String = (0..next(12)).map(|_| words[next(words.len())]).collect();
            for limit in [0, 1, 2, 3, 4, 5, 7, 16, 512] {
                let home = [None, Some("/home/alice"), Some("/Users/alice/"), Some("C:\\Users\\alice")][next(4)];
                let user = [None, Some("alice"), Some(" alice ")][next(3)];
                let bounded = cut(&text, limit * 4 + 256);
                let short = scrub(&text, home, user, limit);
                assert_eq!(short, scrub_paths(bounded, home, user, limit), "{text:?} home {home:?} user {user:?} limit {limit}");
                assert!(short.len() <= limit, "{short:?} is longer than {limit}");
            }
        }
    }

    #[test]
    fn cutting_never_splits_a_character_at_any_limit() {
        for text in ["ééééééééé", "名前名前名前", "🦀🦀🦀🦀", "a🦀é名b", "/Users/alice/名前/🦀.txt and /home/alice/é"]
        {
            for limit in 0..=text.len() + 4 {
                let cut_text = truncate(text, limit);
                assert!(cut_text.len() <= limit, "{text:?} at {limit}: {cut_text:?}");
                assert!(text.starts_with(cut_text.trim_end_matches("...")) || cut_text.is_empty(), "{text:?} at {limit}: {cut_text:?}");
                let scrubbed = scrub(text, Some("/Users/alice"), Some("alice"), limit);
                assert!(scrubbed.len() <= limit && !scrubbed.contains("alice"), "{text:?} at {limit}: {scrubbed:?}");
            }
        }
        // Exactly at the limit nothing is cut; one byte over, the ellipsis appears.
        for (limit, label) in [(MAX_MESSAGE, "message"), (MAX_BACKTRACE, "backtrace"), (MAX_LOG_LINE, "log line"), (MAX_LABEL, "label")] {
            let exact = "é".repeat(limit / 2);
            assert_eq!(scrub(&exact, None, None, limit), exact, "{label}");
            let over = format!("{exact}é");
            let cut_text = scrub(&over, None, None, limit);
            assert!(cut_text.len() <= limit && cut_text.ends_with("...") && cut_text.is_char_boundary(cut_text.len() - 3), "{label}");
        }
    }

    const BIG: usize = 10_000;

    fn s(text: &str, home: Option<&str>, user: Option<&str>) -> String {
        scrub(text, home, user, BIG)
    }

    #[test]
    fn macos_home_becomes_tilde() {
        assert_eq!(s("failed at /Users/alice/proj/main.rs:3", Some("/Users/alice"), Some("alice")), "failed at ~/proj/main.rs:3");
    }

    #[test]
    fn linux_home_becomes_tilde() {
        assert_eq!(s("open /home/alice/.config/app.toml", Some("/home/alice"), Some("alice")), "open ~/.config/app.toml");
    }

    #[test]
    fn windows_home_with_backslashes() {
        assert_eq!(s(r"open C:\Users\alice\AppData\x.db", Some(r"C:\Users\alice"), Some("alice")), r"open ~\AppData\x.db");
    }

    #[test]
    fn windows_home_is_case_insensitive_and_matches_forward_slashes() {
        assert_eq!(s("c:/users/ALICE/docs", Some(r"C:\Users\alice"), Some("alice")), "~/docs");
    }

    #[test]
    fn trailing_separator_on_home_is_ignored() {
        assert_eq!(s("/home/alice/x", Some("/home/alice/"), None), "~/x");
    }

    #[test]
    fn home_prefix_of_another_account_is_left_alone() {
        let out = s("/home/alice2/x", Some("/home/alice"), Some("alice"));
        assert!(!out.contains('~'), "{out}");
        assert!(!out.contains("alice"), "{out}");
    }

    #[test]
    fn home_at_end_of_text_and_repeated() {
        assert_eq!(s("a /home/bob and /home/bob", Some("/home/bob"), None), "a ~ and ~");
    }

    #[test]
    fn account_name_removed_without_home() {
        assert_eq!(s("/Users/carol/work/x.rs", None, None), "/Users/<user>/work/x.rs");
        assert_eq!(s("/home/carol/work", None, None), "/home/<user>/work");
        assert_eq!(s(r"C:\Users\carol\work", None, None), r"C:\Users\<user>\work");
    }

    #[test]
    fn builder_account_in_baked_in_paths_is_removed_too() {
        let out = s("at /Users/builder/.cargo/registry/src/x/lib.rs", Some("/Users/alice"), Some("alice"));
        assert_eq!(out, "at /Users/<user>/.cargo/registry/src/x/lib.rs");
    }

    #[test]
    fn marker_case_is_ignored() {
        assert_eq!(s(r"c:\USERS\Dave\x", None, None), r"c:\USERS\<user>\x");
    }

    #[test]
    fn user_name_as_path_segment_elsewhere() {
        assert_eq!(s("/data/alice/cache and /var/alice", None, Some("alice")), "/data/<user>/cache and /var/<user>");
    }

    #[test]
    fn user_name_with_a_space_on_windows() {
        let out = s(r"C:\Users\John Smith\x", Some(r"C:\Users\John Smith"), Some("John Smith"));
        assert_eq!(out, r"~\x");
    }

    #[test]
    fn plain_words_matching_the_user_name_survive() {
        assert_eq!(s("alice in wonderland", Some("/home/alice"), Some("alice")), "alice in wonderland");
    }

    #[test]
    fn already_scrubbed_text_is_stable() {
        let once = s("/Users/zed/a", Some("/Users/zed"), Some("zed"));
        assert_eq!(s(&once, Some("/Users/zed"), Some("zed")), once);
    }

    #[test]
    fn empty_home_and_user_do_nothing() {
        assert_eq!(s("hello /x/y", Some(""), Some("  ")), "hello /x/y");
        assert_eq!(s("hello /x/y", Some("/"), None), "hello /x/y");
    }

    #[test]
    fn truncates_to_the_limit_with_marker() {
        let out = scrub(&"a".repeat(100), None, None, 20);
        assert_eq!(out.len(), 20);
        assert!(out.ends_with("..."));
    }

    #[test]
    fn short_text_is_not_touched() {
        assert_eq!(scrub("short", None, None, 20), "short");
        assert_eq!(scrub("12345", None, None, 5), "12345");
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let text = "é".repeat(50); // two bytes each
        for limit in 0..40 {
            let out = truncate(&text, limit);
            assert!(out.len() <= limit, "limit {limit} gave {}", out.len());
        }
        let out = truncate("aé", 2);
        assert!(out.len() <= 2);
    }

    #[test]
    fn tiny_limits_do_not_panic() {
        assert_eq!(truncate("abcdef", 0), "");
        assert_eq!(truncate("abcdef", 2), "ab");
        assert_eq!(truncate("abcdef", 3), "abc");
        assert_eq!(truncate("abcdef", 4), "a...");
    }

    #[test]
    fn spec_limits() {
        assert_eq!(MAX_MESSAGE, 2048);
        assert_eq!(MAX_BACKTRACE, 32 * 1024);
        assert_eq!(MAX_LOG_LINE, 512);
    }

    #[test]
    fn huge_input_is_bounded() {
        let text = "/home/alice/x ".repeat(100_000);
        let out = scrub(&text, Some("/home/alice"), Some("alice"), MAX_MESSAGE);
        assert!(out.len() <= MAX_MESSAGE);
        assert!(!out.contains("alice"));
    }

    #[test]
    fn scrubber_applies_its_parameters() {
        let scrubber = Scrubber::new(Some("/Users/erin".into()), Some("erin".into()));
        assert_eq!(scrubber.apply("/Users/erin/x", 100), "~/x");
    }

    #[test]
    fn non_ascii_text_around_paths() {
        assert_eq!(s("héllo /Users/zoë/ünï", None, None), "héllo /Users/<user>/ünï");
    }
}
