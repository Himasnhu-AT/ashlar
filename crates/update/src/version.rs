//! Semantic versions (`MAJOR.MINOR.PATCH[-prerelease][+build]`), ordered the way semver.org orders
//! them. A leading `v` is accepted (`v1.2.3`); build metadata is accepted and ignored.

use std::cmp::Ordering;
use std::fmt;

/// The longest version text accepted.
pub const MAX_VERSION_LEN: usize = 64;

/// A parsed semantic version.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Dot-separated pre-release identifiers; empty for a release.
    pub pre: Vec<String>,
}

/// A numeric identifier: digits only, no leading zero (semver), fits in a `u64`.
fn number(part: &str) -> Option<u64> {
    if part.is_empty() || (part.len() > 1 && part.starts_with('0')) || !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

fn identifier_ok(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

impl Version {
    /// Parses `text` (surrounding white space allowed). `None` when it is not a semantic version.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        if text.is_empty() || text.len() > MAX_VERSION_LEN {
            return None;
        }
        let text = match text.split_once('+') {
            Some((core, build)) => {
                if !build.split('.').all(identifier_ok) {
                    return None;
                }
                core
            }
            None => text,
        };
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut parts = core.split('.');
        let (major, minor, patch) = (number(parts.next()?)?, number(parts.next()?)?, number(parts.next()?)?);
        if parts.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => {
                let ids: Vec<String> = pre.split('.').map(str::to_string).collect();
                let ok = ids.iter().all(|id| identifier_ok(id) && (!id.bytes().all(|b| b.is_ascii_digit()) || number(id).is_some()));
                if !ok {
                    return None;
                }
                ids
            }
        };
        Some(Self { major, minor, patch, pre })
    }

    /// A pre-release (`1.2.0-beta.1`).
    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch)).then_with(|| {
            match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                // a pre-release comes before its release
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (a, b) in self.pre.iter().zip(&other.pre) {
                        let order = match (number(a), number(b)) {
                            (Some(a), Some(b)) => a.cmp(&b),
                            // numeric identifiers sort before alphanumeric ones
                            (Some(_), None) => Ordering::Less,
                            (None, Some(_)) => Ordering::Greater,
                            (None, None) => a.cmp(b),
                        };
                        if order != Ordering::Equal {
                            return order;
                        }
                    }
                    self.pre.len().cmp(&other.pre.len())
                }
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn parses_core_prerelease_build_and_v_prefix() {
        assert_eq!(v("1.2.3"), Version { major: 1, minor: 2, patch: 3, pre: vec![] });
        assert_eq!(v("v0.10.0").minor, 10);
        assert_eq!(v(" 1.0.0-rc.1 ").pre, vec!["rc", "1"]);
        assert_eq!(v("1.0.0+build.7"), v("1.0.0"));
        assert_eq!(v("2.0.0-beta+exp.sha.5114f85").to_string(), "2.0.0-beta");
    }

    #[test]
    fn refuses_what_is_not_semver() {
        for bad in ["", "1", "1.2", "1.2.3.4", "01.2.3", "1.02.3", "-1.2.3", "1.2.3-", "1.2.3-01", "1.2.3-a..b", "1.2.3+", "1.2.x", "a.b.c"] {
            assert!(Version::parse(bad).is_none(), "{bad}");
        }
        assert!(Version::parse(&"1".repeat(65)).is_none());
        assert!(Version::parse("99999999999999999999.0.0").is_none());
    }

    #[test]
    fn orders_as_semver_org() {
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "2.0.0",
            "10.0.0",
        ];
        for pair in chain.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
        assert!(v("0.9.10") > v("0.9.9"));
        assert_eq!(v("1.0.0").cmp(&v("1.0.0+other")), Ordering::Equal);
    }
}
