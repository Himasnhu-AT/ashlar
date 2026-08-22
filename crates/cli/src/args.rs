//! A tiny argument parser: flags (`--json`), options (`--connect 7981`, `--connect=7981`) and
//! positionals, taken in any order. Whatever a verb does not take is an error at [`Args::finish`].

/// The arguments after the verb.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Args {
    items: Vec<String>,
    /// Items after a bare `--` are positionals even when they start with `--`.
    rest: Vec<String>,
}

impl Args {
    pub fn new(items: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut all: Vec<String> = items.into_iter().map(Into::into).collect();
        let rest = match all.iter().position(|a| a == "--") {
            Some(i) => {
                let tail = all.split_off(i);
                tail.into_iter().skip(1).collect()
            }
            None => Vec::new(),
        };
        Self { items: all, rest }
    }

    /// Take `--name` if present.
    pub fn flag(&mut self, name: &str) -> bool {
        match self.items.iter().position(|a| a == name) {
            Some(i) => {
                self.items.remove(i);
                true
            }
            None => false,
        }
    }

    /// Take `--name VALUE` or `--name=VALUE`.
    pub fn value(&mut self, name: &str) -> Result<Option<String>, String> {
        let prefix = format!("{name}=");
        if let Some(i) = self.items.iter().position(|a| a.starts_with(&prefix)) {
            let a = self.items.remove(i);
            return Ok(Some(a.get(prefix.len()..).unwrap_or("").to_string()));
        }
        let Some(i) = self.items.iter().position(|a| a == name) else { return Ok(None) };
        self.items.remove(i);
        if i < self.items.len() { Ok(Some(self.items.remove(i))) } else { Err(format!("{name} needs a value")) }
    }

    /// Take `--name N` as a number.
    pub fn number<T: std::str::FromStr>(&mut self, name: &str) -> Result<Option<T>, String> {
        match self.value(name)? {
            Some(v) => v.parse().map(Some).map_err(|_| format!("{name}: `{v}` is not a valid number")),
            None => Ok(None),
        }
    }

    /// Take the next positional (`-` counts as one: stdin). Take options first, so an option's
    /// value is not mistaken for a positional.
    pub fn positional(&mut self) -> Option<String> {
        match self.items.iter().position(|a| !a.starts_with("--")) {
            Some(i) => Some(self.items.remove(i)),
            None => (!self.rest.is_empty()).then(|| self.rest.remove(0)),
        }
    }

    /// Everything was taken; otherwise the first leftover is an error.
    pub fn finish(self) -> Result<(), String> {
        match self.items.first().or(self.rest.first()) {
            None => Ok(()),
            Some(a) if a.starts_with("--") => Err(format!("unknown option {a}")),
            Some(a) => Err(format!("unexpected argument `{a}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_values_and_positionals_in_any_order() {
        let mut a = Args::new(["--json", "doc.add", "--connect=7981", "{\"x\":1}", "--depth", "3"]);
        assert!(a.flag("--json"));
        assert!(!a.flag("--json"));
        assert_eq!(a.number::<u16>("--connect"), Ok(Some(7981)));
        assert_eq!(a.number::<u32>("--depth"), Ok(Some(3)));
        assert_eq!(a.positional().as_deref(), Some("doc.add"));
        assert_eq!(a.positional().as_deref(), Some("{\"x\":1}"));
        assert_eq!(a.positional(), None);
        assert_eq!(a.finish(), Ok(()));
    }

    #[test]
    fn leftovers_and_missing_values_are_errors() {
        assert_eq!(Args::new(["--nope"]).finish(), Err("unknown option --nope".into()));
        assert_eq!(Args::new(["extra"]).finish(), Err("unexpected argument `extra`".into()));
        assert_eq!(Args::new(["--connect"]).value("--connect"), Err("--connect needs a value".into()));
        assert!(Args::new(["--connect", "x"]).number::<u16>("--connect").is_err());
        assert!(Args::new(["--connect", "70000"]).number::<u16>("--connect").is_err());
    }

    #[test]
    fn double_dash_ends_options() {
        let mut a = Args::new(["exec", "--", "--weird-id"]);
        assert_eq!(a.positional().as_deref(), Some("exec"));
        assert_eq!(a.positional().as_deref(), Some("--weird-id"));
        assert_eq!(a.finish(), Ok(()));
        let mut a = Args::new(["-"]);
        assert_eq!(a.positional().as_deref(), Some("-"));
    }
}
