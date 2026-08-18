//! `cmd!` / `query!`: the shape of existing app command tables, so they port by search and
//! replace.

/// A mutating command:
/// `cmd!(id, label, [menu…], shortcut, params, enabled, run)`.
///
/// `params` is a legacy text doc (`r#"{"name":str}"#`) or a typed list
/// (`[Param::string("name").required()]`, built in a `const` so the table can be `static`).
/// `run` returns the host's [`crate::Host::Error`].
///
/// ```
/// use ashlar_command::{CommandError, CommandSpec, Host, Journal, Param, always, cmd};
/// struct Doc { title: String, journal: Journal }
/// impl Host for Doc {
///     type Error = CommandError;
///     fn journal(&mut self) -> &mut Journal { &mut self.journal }
/// }
/// let spec: CommandSpec<Doc> = cmd!("doc.rename", "Rename…", ["Edit"], Some("Mod+R"), [Param::string("title").required()], always, |d, p| {
///     d.title = p["title"].as_str().unwrap_or_default().to_string();
///     Ok(serde_json::Value::Null)
/// });
/// assert!(spec.mutates);
/// ```
#[macro_export]
macro_rules! cmd {
    ($id:literal, $label:literal, [$($m:literal),* $(,)?], $sc:expr, [$($p:expr),* $(,)?], $en:expr, $run:expr $(,)?) => {
        $crate::CommandSpec {
            id: $id,
            label: $label,
            menu: &[$($m),*],
            shortcut: $sc,
            params: $crate::Params::Static({
                const PARAMS: &[$crate::Param] = &[$($p),*];
                PARAMS
            }),
            enabled: $en,
            run: $run,
            mutates: true,
        }
    };
    ($id:literal, $label:literal, [$($m:literal),* $(,)?], $sc:expr, $params:literal, $en:expr, $run:expr $(,)?) => {
        $crate::CommandSpec {
            id: $id,
            label: $label,
            menu: &[$($m),*],
            shortcut: $sc,
            params: $crate::Params::Text($params),
            enabled: $en,
            run: $run,
            mutates: true,
        }
    };
}

/// A pure query (always enabled, not in menus, not journaled):
/// `query!(id, label, params, run)`.
#[macro_export]
macro_rules! query {
    ($id:literal, $label:literal, [$($p:expr),* $(,)?], $run:expr $(,)?) => {
        $crate::CommandSpec {
            id: $id,
            label: $label,
            menu: &[],
            shortcut: None,
            params: $crate::Params::Static({
                const PARAMS: &[$crate::Param] = &[$($p),*];
                PARAMS
            }),
            enabled: $crate::always,
            run: $run,
            mutates: false,
        }
    };
    ($id:literal, $label:literal, $params:literal, $run:expr $(,)?) => {
        $crate::CommandSpec {
            id: $id,
            label: $label,
            menu: &[],
            shortcut: None,
            params: $crate::Params::Text($params),
            enabled: $crate::always,
            run: $run,
            mutates: false,
        }
    };
}
