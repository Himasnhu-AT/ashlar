//! `cargo xtask names`: no app name may appear in the published crates (AGENTS.md §1).
//!
//! The names are kept as FNV-1a hashes so this file does not itself contain them. A long, distinctive
//! name matches anywhere inside a word (`MyNameDoc` too); a short one that is also an English word
//! matches only as a whole word or camel-case part (`name-ui`, `name_doc`, `NameApp`), never inside a
//! longer word. False positives go in `xtask/names-allow.txt`.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Match {
    /// Anywhere in a word, case-insensitive.
    Within,
    /// A whole word: split at non-alphanumerics, camel-case humps and digits.
    Word,
}

/// (FNV-1a of the lowercase name, its length in bytes, how it matches).
pub const FORBIDDEN: &[(u64, usize, Match)] = &[
    (0xf75016fbb527aa8f, 9, Match::Within),
    (0x82757cee3dfc919e, 11, Match::Within),
    (0x49b095d2fab9b11e, 10, Match::Within),
    (0x1dabf1bac7119289, 9, Match::Within),
    (0x9c89d87215276ff3, 8, Match::Within),
    (0x457dc19057ca90de, 9, Match::Within),
    (0xa3e02073f071feb3, 8, Match::Within),
    (0x55d9341755d5dfce, 8, Match::Within),
    (0xf81f30fde4da1bb0, 5, Match::Within),
    (0x00392b19133936cf, 3, Match::Word),
    // the other suite apps
    (0xcdcae78cb1175f9b, 11, Match::Within),
    (0xfc96b95dd7f5dfcc, 11, Match::Within),
    (0x406d6b25a9caef87, 10, Match::Within),
    (0xa29f3fb230bb0051, 8, Match::Within),
    (0x486cb51beedff37b, 10, Match::Within),
    (0xc55c37eaee936ef5, 9, Match::Within),
];

/// What is scanned, relative to the workspace root: everything that is published or shown to users.
const SCANNED_DIRS: &[&str] = &["crates", "examples"];
const SCANNED_FILES: &[&str] = &["README.md", "Cargo.toml"];
const SCANNED_EXTENSIONS: &[&str] = &["rs", "md", "toml", "txt", "json", "yml", "yaml", "html", "css", "wgsl", "ftl", "hbs", "js", "svg"];
const SKIPPED_DIRS: &[&str] = &["target", ".git", "node_modules"];
const ALLOW_FILE: &str = "xtask/names-allow.txt";

pub fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

/// One forbidden name found in a line.
#[derive(Debug, PartialEq, Eq)]
pub struct Hit {
    /// The whole word (lowercase) the name was found in; what an allow-list entry names.
    pub word: String,
    /// 1-based column of the word.
    pub column: usize,
}

/// One allow-list entry: `word` alone allows it everywhere; `path word` allows it in files whose
/// workspace-relative path starts with `path`.
#[derive(Debug, PartialEq, Eq)]
pub struct Allow {
    pub path: Option<String>,
    pub word: String,
}

pub fn parse_allow(text: &str) -> Vec<Allow> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or_default().trim())
        .filter(|l| !l.is_empty())
        .map(|l| match l.rsplit_once(char::is_whitespace) {
            Some((path, word)) => Allow { path: Some(path.trim().replace('\\', "/")), word: word.to_ascii_lowercase() },
            None => Allow { path: None, word: l.to_ascii_lowercase() },
        })
        .collect()
}

fn allowed(allow: &[Allow], path: &str, word: &str) -> bool {
    allow.iter().any(|a| a.word == word && a.path.as_deref().is_none_or(|p| path.starts_with(p)))
}

/// The parts of a word, split at camel-case humps and letter/digit boundaries: `FooBar2x` gives
/// `foo`, `bar`, `2`, `x`; `HTTPServer` gives `http`, `server`.
fn parts(word: &str) -> Vec<String> {
    let chars: Vec<char> = word.chars().collect();
    let mut out = Vec::new();
    let mut current = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let prev = i.checked_sub(1).and_then(|p| chars.get(p)).copied();
        let next = chars.get(i + 1).copied();
        let boundary = match prev {
            None => false,
            Some(p) => {
                (p.is_ascii_lowercase() && c.is_ascii_uppercase())
                    || (p.is_ascii_digit() != c.is_ascii_digit())
                    || (p.is_ascii_uppercase() && c.is_ascii_uppercase() && next.is_some_and(|n| n.is_ascii_lowercase()))
            }
        };
        if boundary && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        current.push(c.to_ascii_lowercase());
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// The forbidden names in one line of text.
pub fn scan_line(line: &str, forbidden: &[(u64, usize, Match)]) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut column = 0;
    for word in line.split(|c: char| !c.is_ascii_alphanumeric()) {
        let start = column;
        column += word.len() + 1;
        if word.is_empty() {
            continue;
        }
        let lower = word.to_ascii_lowercase();
        let found = forbidden.iter().any(|&(hash, len, how)| match how {
            Match::Word => parts(word).iter().any(|p| p.len() == len && fnv1a(p) == hash),
            Match::Within => (0..=lower.len().saturating_sub(len)).any(|i| lower.get(i..i + len).is_some_and(|s| fnv1a(s) == hash)),
        });
        if found {
            hits.push(Hit { word: lower, column: start + 1 });
        }
    }
    hits
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !SKIPPED_DIRS.contains(&name.as_str()) {
                files(&path, out);
            }
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| SCANNED_EXTENSIONS.contains(&e)) {
            out.push(path);
        }
    }
}

pub fn check(root: &Path) -> Result<(), String> {
    let allow = parse_allow(&std::fs::read_to_string(root.join(ALLOW_FILE)).unwrap_or_default());
    let mut paths = Vec::new();
    for dir in SCANNED_DIRS {
        files(&root.join(dir), &mut paths);
    }
    paths.extend(SCANNED_FILES.iter().map(|f| root.join(f)).filter(|p| p.is_file()));
    paths.sort();
    let mut errors = Vec::new();
    for path in &paths {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/");
        for (n, line) in text.lines().enumerate() {
            for hit in scan_line(line, FORBIDDEN) {
                if !allowed(&allow, &rel, &hit.word) {
                    // Only the first letter: the log should not spell the name out either.
                    let first = hit.word.chars().next().unwrap_or('?');
                    errors.push(format!(
                        "{rel}:{}:{}: an app name in a word starting with {first:?} (if it is a false positive, add `{rel} <word>` to {ALLOW_FILE})",
                        n + 1,
                        hit.column
                    ));
                }
            }
        }
    }
    if errors.is_empty() {
        println!("names: ok ({} files)", paths.len());
        Ok(())
    } else {
        Err(format!("names: {} app name(s) in public crates (AGENTS.md §1)\n{}", errors.len(), errors.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> Vec<(u64, usize, Match)> {
        vec![(fnv1a("zorkmid"), 7, Match::Within), (fnv1a("gru"), 3, Match::Word)]
    }

    fn words(line: &str) -> Vec<String> {
        scan_line(line, &list()).into_iter().map(|h| h.word).collect()
    }

    #[test]
    fn long_names_match_inside_words_case_insensitively() {
        assert_eq!(words("use ZorkmidDoc; // zorkmid_ui, myzorkmids"), ["zorkmiddoc", "zorkmid", "myzorkmids"]);
    }

    #[test]
    fn short_names_match_whole_words_and_camel_parts_only() {
        assert_eq!(words("gru-ui gru_doc GruApp myGru Gru2 HTTPGru"), ["gru", "gru", "gruapp", "mygru", "gru2", "httpgru"]);
        assert!(words("grub gruel ogrus angry").is_empty());
    }

    #[test]
    fn columns_are_one_based() {
        let hits = scan_line("a gru", &list());
        assert_eq!(hits, [Hit { word: "gru".into(), column: 3 }]);
    }

    #[test]
    fn camel_parts() {
        assert_eq!(parts("HTTPServer2x"), ["http", "server", "2", "x"]);
        assert_eq!(parts("fooBar"), ["foo", "bar"]);
    }

    #[test]
    fn allow_list_by_word_and_path() {
        let allow = parse_allow("# comment\ngruel\ncrates/a/src gruesome  # trailing\n\n");
        assert_eq!(allow.len(), 2);
        assert!(allowed(&allow, "crates/z/x.rs", "gruel"));
        assert!(allowed(&allow, "crates/a/src/lib.rs", "gruesome"));
        assert!(!allowed(&allow, "crates/b/src/lib.rs", "gruesome"));
    }

    #[test]
    fn the_real_list_is_well_formed() {
        assert!(FORBIDDEN.iter().all(|&(_, len, _)| len >= 3));
        assert!(scan_line("ashlar store command jobs", FORBIDDEN).is_empty());
    }
}
