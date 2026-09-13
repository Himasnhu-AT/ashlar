//! Put an app's bundled command-line tool on the user's `PATH`, the way editors offer an
//! "Install command" menu item.
//!
//! The tool ships inside the app (on macOS `<App>.app/Contents/Helpers/<name>`). Installing makes
//! a symbolic link `<bin dir>/<name>` → that file, so updating the app updates the tool:
//!
//! 1. The link goes in `/usr/local/bin` (created when missing).
//! 2. When that folder isn't writable, the OS asks for an administrator password (macOS: the
//!    standard `do shell script … with administrator privileges` prompt).
//! 3. When that is declined (or no prompt is available), the link goes in `~/.local/bin` and the
//!    result carries the line to add to the shell profile.
//!
//! A file or link that isn't this app's tool is never replaced unless the caller passes
//! `replace` (and the result names what was replaced). A link left behind after the app moved
//! (it points at a missing `…/Contents/Helpers/<name>`) is repaired without asking. A folder is
//! never replaced.
//!
//! [`ENV_BIN_DIR`] and [`ENV_FALLBACK_DIR`] point both folders elsewhere for tests; while
//! either is set the administrator prompt is never shown.
//!
//! [`commands`] are the matching command specs (`app.installCommandLineTool`,
//! `app.uninstallCommandLineTool`, `app.commandLineToolStatus`) for any host that implements
//! [`CliToolHost`].

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ashlar_command::{CommandError, CommandSpec, Host, Param, Params};
use serde::Serialize;
use serde_json::{Value, json};

/// Where the link goes by default: on every macOS `PATH` (`/etc/paths`).
pub const SYSTEM_BIN_DIR: &str = "/usr/local/bin";
/// Install into this folder instead of [`SYSTEM_BIN_DIR`] (tests); disables the admin prompt.
pub const ENV_BIN_DIR: &str = "ASHLAR_CLI_BIN_DIR";
/// Fall back to this folder instead of `~/.local/bin` (tests); disables the admin prompt.
pub const ENV_FALLBACK_DIR: &str = "ASHLAR_CLI_FALLBACK_DIR";

/// Command ids registered by [`commands`].
pub const INSTALL: &str = "app.installCommandLineTool";
pub const UNINSTALL: &str = "app.uninstallCommandLineTool";
pub const STATUS: &str = "app.commandLineToolStatus";

/// Why installing or removing the tool did not happen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CliToolError {
    /// The tool is not where the app expects it (a development build, a damaged copy).
    #[error("the command-line tool isn't in this copy of the app (expected at {0})")]
    HelperMissing(PathBuf),
    /// Something that isn't this app's tool is in the way; nothing was changed.
    #[error("{} already exists and {}; nothing was changed{}", link.display(), what, if *replaceable { " (pass `replace: true` to replace it)" } else { "" })]
    Conflict { link: PathBuf, what: String, replaceable: bool },
    /// The name or a path can't be used.
    #[error("invalid: {0}")]
    Invalid(String),
    /// Not available on this platform.
    #[error("not supported on this platform")]
    Unsupported,
    /// The OS refused; one line, safe to show.
    #[error("{0}")]
    Os(String),
}

impl CliToolError {
    pub fn is_conflict(&self) -> bool {
        matches!(self, Self::Conflict { .. })
    }
}

impl From<CliToolError> for CommandError {
    fn from(e: CliToolError) -> Self {
        match e {
            CliToolError::Invalid(_) => CommandError::bad_params("", e.to_string()),
            _ => CommandError::failed("", e.to_string()),
        }
    }
}

type Result<T> = std::result::Result<T, CliToolError>;

/// What one location holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum LinkState {
    /// Nothing there.
    Missing,
    /// A link to this app's tool.
    Ours,
    /// A link to a missing `…/Contents/Helpers/<name>`: the app moved or was deleted. Repaired
    /// by installing.
    Broken { target: PathBuf },
    /// A link to the same tool in another copy of the app.
    OtherCopy { target: PathBuf },
    /// A link to something else, or a broken link that was never ours.
    ForeignLink { target: PathBuf },
    /// A regular file.
    File,
    /// A folder (never replaced).
    Folder,
}

impl LinkState {
    fn is_ours_or_stale(&self) -> bool {
        matches!(self, Self::Ours | Self::Broken { .. })
    }
}

/// Where the tool is installed, if anywhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// The location that was looked at: the system folder, or the fallback when the system one
    /// is empty and the fallback isn't.
    pub link: PathBuf,
    #[serde(flatten)]
    pub state: LinkState,
    /// `link` is in the fallback folder.
    pub fallback: bool,
    pub helper: PathBuf,
}

impl Status {
    pub fn is_installed(&self) -> bool {
        self.state == LinkState::Ours
    }
    /// The link is stale (the app moved): installing repairs it.
    pub fn needs_repair(&self) -> bool {
        matches!(self.state, LinkState::Broken { .. })
    }
    /// One sentence for a status line.
    pub fn message(&self, name: &str) -> String {
        let link = self.link.display();
        match &self.state {
            LinkState::Missing => format!("`{name}` isn't installed."),
            LinkState::Ours => format!("`{name}` is installed at {link}."),
            LinkState::Broken { target } => format!("{link} points to {}, which no longer exists. Install again to repair it.", target.display()),
            LinkState::OtherCopy { target } => format!("{link} points to another copy of the app ({}).", target.display()),
            LinkState::ForeignLink { target } => format!("{link} is a link to {}, which isn't this app's tool.", target.display()),
            LinkState::File => format!("{link} is a file that isn't this app's tool."),
            LinkState::Folder => format!("{link} is a folder."),
        }
    }
}

/// What [`CliTool::install`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Installed {
    pub link: PathBuf,
    /// Installed in the fallback folder (the admin prompt was declined or unavailable).
    pub fallback: bool,
    /// Already installed; nothing changed.
    pub unchanged: bool,
    /// A stale link was repaired.
    pub repaired: bool,
    /// What was replaced because the caller passed `replace` (a file, or a link's target).
    pub replaced: Option<String>,
    /// The administrator prompt was used.
    pub elevated: bool,
    /// For a fallback install: the line to add to the shell profile.
    pub path_hint: Option<String>,
}

impl Installed {
    pub fn message(&self, name: &str) -> String {
        let link = self.link.display();
        let mut text = if self.unchanged {
            format!("`{name}` is already installed at {link}.")
        } else if self.repaired {
            format!("Repaired `{name}` at {link}.")
        } else {
            format!("Installed `{name}` at {link}.")
        };
        if let Some(r) = &self.replaced {
            text.push_str(&format!(" It replaced {r}."));
        }
        if let Some(hint) = &self.path_hint {
            text.push_str(&format!(" If your shell can't find `{name}`, add this line to ~/.zshrc: {hint}"));
        }
        text
    }
}

/// What [`CliTool::uninstall`] did.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Removed {
    pub removed: Vec<PathBuf>,
    /// Left in place, with the reason (not ours, or the admin prompt was declined).
    pub kept: Vec<(PathBuf, String)>,
}

impl Removed {
    pub fn message(&self, name: &str) -> String {
        let mut parts = vec![];
        if self.removed.is_empty() && self.kept.is_empty() {
            parts.push(format!("`{name}` wasn't installed."));
        }
        for p in &self.removed {
            parts.push(format!("Removed {}.", p.display()));
        }
        for (p, why) in &self.kept {
            parts.push(format!("Left {} in place: {why}.", p.display()));
        }
        parts.join(" ")
    }
}

/// The answer to an administrator prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Elevated {
    /// The script ran as administrator and exited successfully.
    Done,
    /// The user cancelled, or no prompt is available.
    Declined,
    /// The script ran and failed, or the prompt couldn't be shown; one line.
    Failed(String),
}

/// Runs a `/bin/sh` script with administrator rights. The script is built by this module from
/// fixed commands and single-quoted paths.
pub type Elevator = Arc<dyn Fn(&str) -> Elevated + Send + Sync>;

/// How to get administrator rights when the system folder isn't writable.
#[derive(Clone)]
pub enum Elevation {
    /// The OS's own password prompt (macOS); elsewhere the same as `Never`.
    Prompt,
    /// Never ask: fall back straight away.
    Never,
    /// A caller-supplied runner (tests, or an app with its own privileged helper).
    Custom(Elevator),
}

impl std::fmt::Debug for Elevation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Prompt => "Prompt",
            Self::Never => "Never",
            Self::Custom(_) => "Custom",
        })
    }
}

/// One app's bundled command-line tool and where to install it.
#[derive(Debug, Clone)]
pub struct CliTool {
    name: String,
    helper: PathBuf,
    bin_dir: PathBuf,
    fallback_dir: Option<PathBuf>,
    elevation: Elevation,
    prompt: String,
}

/// Longest tool name accepted.
const MAX_NAME: usize = 64;

/// A tool name: lowercase ASCII letters, digits, `-`, `_`, `.`; not starting with `-` or `.`.
pub fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.starts_with(['-', '.'])
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(CliToolError::Invalid(format!("`{}` is not a usable command name", name.chars().take(MAX_NAME).collect::<String>())))
    }
}

/// The bundled tool's path for the running app: `<exe dir>/../Helpers/<name>`, which on macOS is
/// `<App>.app/Contents/Helpers/<name>`.
pub fn bundled_helper(name: &str) -> Result<PathBuf> {
    check_name(name)?;
    let exe = std::env::current_exe().map_err(|e| CliToolError::Os(crate::os_text(e)))?;
    let contents = exe.parent().and_then(Path::parent).ok_or_else(|| CliToolError::Os("the app's own path has no parent folder".into()))?;
    Ok(contents.join("Helpers").join(name))
}

impl CliTool {
    /// The tool `name` at `helper`, installed into [`SYSTEM_BIN_DIR`] with `~/.local/bin` as the
    /// fallback and the OS's admin prompt; [`ENV_BIN_DIR`] / [`ENV_FALLBACK_DIR`] override the
    /// folders (and turn the prompt off).
    pub fn new(name: &str, helper: impl Into<PathBuf>) -> Result<Self> {
        check_name(name)?;
        let env_dir = |key: &str| std::env::var_os(key).filter(|v| !v.is_empty()).map(PathBuf::from);
        let (bin, fallback) = (env_dir(ENV_BIN_DIR), env_dir(ENV_FALLBACK_DIR));
        let overridden = bin.is_some() || fallback.is_some();
        let home_fallback = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".local/bin"));
        Ok(Self {
            name: name.to_string(),
            helper: helper.into(),
            bin_dir: bin.unwrap_or_else(|| PathBuf::from(SYSTEM_BIN_DIR)),
            fallback_dir: fallback.or(home_fallback),
            elevation: if overridden { Elevation::Never } else { Elevation::Prompt },
            prompt: format!("Install the `{name}` command."),
        })
    }

    /// [`CliTool::new`] with the running app's [`bundled_helper`].
    pub fn for_current_app(name: &str) -> Result<Self> {
        Self::new(name, bundled_helper(name)?)
    }

    /// Install into `dir` instead of the system folder.
    pub fn bin_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.bin_dir = dir.into();
        self
    }

    /// Fall back to `dir` (`None`: no fallback, a declined prompt is an error).
    pub fn fallback_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.fallback_dir = dir;
        self
    }

    pub fn elevation(mut self, elevation: Elevation) -> Self {
        self.elevation = elevation;
        self
    }

    /// The text shown in the administrator prompt.
    pub fn prompt(mut self, text: impl Into<String>) -> Self {
        self.prompt = text.into();
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn helper(&self) -> &Path {
        &self.helper
    }

    /// The link in the system folder.
    pub fn link(&self) -> PathBuf {
        self.bin_dir.join(&self.name)
    }

    fn fallback_link(&self) -> Option<PathBuf> {
        self.fallback_dir.as_ref().map(|d| d.join(&self.name))
    }

    /// The tool exists in the app.
    pub fn helper_present(&self) -> bool {
        self.helper.is_file()
    }

    /// What `link` holds.
    pub fn state_of(&self, link: &Path) -> LinkState {
        let Ok(meta) = std::fs::symlink_metadata(link) else { return LinkState::Missing };
        if meta.is_dir() {
            return LinkState::Folder;
        }
        if !meta.file_type().is_symlink() {
            return LinkState::File;
        }
        let Ok(raw) = std::fs::read_link(link) else { return LinkState::File };
        let target = if raw.is_absolute() { raw } else { link.parent().map_or(raw.clone(), |p| p.join(&raw)) };
        if same_file(&target, &self.helper) {
            return LinkState::Ours;
        }
        let ends_like_ours = target.file_name().is_some_and(|n| n == self.name.as_str())
            && target.parent().and_then(Path::file_name).is_some_and(|n| n == "Helpers")
            && target.parent().and_then(Path::parent).and_then(Path::file_name).is_some_and(|n| n == "Contents");
        match (ends_like_ours, target.exists()) {
            (true, false) => LinkState::Broken { target },
            (true, true) => LinkState::OtherCopy { target },
            (false, _) => LinkState::ForeignLink { target },
        }
    }

    /// Where the tool is installed: the system link, or the fallback one when the system folder
    /// holds nothing and the fallback does.
    pub fn status(&self) -> Status {
        let link = self.link();
        let state = self.state_of(&link);
        if state == LinkState::Missing
            && let Some(fb) = self.fallback_link()
        {
            let fb_state = self.state_of(&fb);
            if fb_state != LinkState::Missing {
                return Status { link: fb, state: fb_state, fallback: true, helper: self.helper.clone() };
            }
        }
        Status { link, state, fallback: false, helper: self.helper.clone() }
    }

    /// Link the tool into the system folder (asking for admin rights when needed), or into the
    /// fallback when that is declined. Repairs a stale link. `replace` allows replacing a file or
    /// a link that isn't ours (never a folder).
    pub fn install(&self, replace: bool) -> Result<Installed> {
        if !self.helper_present() {
            return Err(CliToolError::HelperMissing(self.helper.clone()));
        }
        let link = self.link();
        let state = self.state_of(&link);
        let done = |link: PathBuf, fallback: bool, elevated: bool, state: &LinkState| Installed {
            link,
            fallback,
            unchanged: false,
            repaired: matches!(state, LinkState::Broken { .. }),
            replaced: replaced_text(state),
            elevated,
            path_hint: None,
        };
        if state == LinkState::Ours {
            let mut out = done(link, false, false, &state);
            out.unchanged = true;
            return Ok(out);
        }
        self.check_replaceable(&link, &state, replace)?;
        match link_atomically(&self.helper, &link) {
            Ok(()) => return Ok(done(link, false, false, &state)),
            Err(e) if e.kind() == io::ErrorKind::Unsupported => return Err(CliToolError::Unsupported),
            Err(e) if e.kind() != io::ErrorKind::PermissionDenied => return Err(CliToolError::Os(crate::os_text(e))),
            Err(_) => {}
        }
        let script = format!("/bin/mkdir -p {} && /bin/ln -sfn {} {}", sh_quote(&self.bin_dir)?, sh_quote(&self.helper)?, sh_quote(&link)?);
        match self.elevate(&script) {
            Elevated::Done if self.state_of(&link) == LinkState::Ours => return Ok(done(link, false, true, &state)),
            Elevated::Done => return Err(CliToolError::Os(format!("{} wasn't created as administrator", link.display()))),
            Elevated::Failed(why) => return Err(CliToolError::Os(why)),
            Elevated::Declined => {}
        }
        let Some(fb) = self.fallback_link() else {
            return Err(CliToolError::Os(format!("{} isn't writable and administrator rights weren't given", self.bin_dir.display())));
        };
        let fb_state = self.state_of(&fb);
        let hint = Some(path_hint(fb.parent().unwrap_or(&fb)));
        if fb_state == LinkState::Ours {
            let mut out = done(fb, true, false, &fb_state);
            out.unchanged = true;
            out.path_hint = hint;
            return Ok(out);
        }
        self.check_replaceable(&fb, &fb_state, replace)?;
        link_atomically(&self.helper, &fb).map_err(|e| CliToolError::Os(crate::os_text(e)))?;
        let mut out = done(fb, true, false, &fb_state);
        out.path_hint = hint;
        Ok(out)
    }

    /// Install only when the link is stale (the app moved); otherwise report the status.
    pub fn repair(&self) -> Result<Option<Installed>> {
        if self.status().needs_repair() { self.install(false).map(Some) } else { Ok(None) }
    }

    /// Remove this app's links (and stale ones) from both folders. Anything else stays.
    pub fn uninstall(&self) -> Result<Removed> {
        let mut out = Removed::default();
        let links = std::iter::once(self.link()).chain(self.fallback_link());
        for link in links {
            let state = self.state_of(&link);
            if state == LinkState::Missing {
                continue;
            }
            if !state.is_ours_or_stale() {
                let why = match &state {
                    LinkState::OtherCopy { .. } => "it belongs to another copy of the app",
                    _ => "it isn't this app's tool",
                };
                out.kept.push((link, why.into()));
                continue;
            }
            match std::fs::remove_file(&link) {
                Ok(()) => out.removed.push(link),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => match self.elevate(&format!("/bin/rm -f {}", sh_quote(&link)?)) {
                    Elevated::Done if self.state_of(&link) == LinkState::Missing => out.removed.push(link),
                    Elevated::Done => out.kept.push((link, "it couldn't be removed as administrator".into())),
                    Elevated::Declined => out.kept.push((link, "removing it needs administrator rights".into())),
                    Elevated::Failed(why) => out.kept.push((link, why)),
                },
                Err(e) => out.kept.push((link, crate::os_text(e))),
            }
        }
        Ok(out)
    }

    fn check_replaceable(&self, link: &Path, state: &LinkState, replace: bool) -> Result<()> {
        let what = match state {
            LinkState::Missing | LinkState::Ours | LinkState::Broken { .. } => return Ok(()),
            LinkState::Folder => return Err(CliToolError::Conflict { link: link.into(), what: "is a folder".into(), replaceable: false }),
            _ if replace => return Ok(()),
            LinkState::OtherCopy { target } => format!("points to another copy of the app ({})", target.display()),
            LinkState::ForeignLink { target } => format!("is a link to {}", target.display()),
            LinkState::File => "is a file that isn't this app's tool".into(),
        };
        Err(CliToolError::Conflict { link: link.into(), what, replaceable: true })
    }

    fn elevate(&self, script: &str) -> Elevated {
        match &self.elevation {
            Elevation::Never => Elevated::Declined,
            Elevation::Custom(f) => f(script),
            Elevation::Prompt => crate::modal::around(|| admin_prompt(script, &self.prompt)),
        }
    }
}

fn replaced_text(state: &LinkState) -> Option<String> {
    match state {
        LinkState::OtherCopy { target } | LinkState::ForeignLink { target } => Some(format!("a link to {}", target.display())),
        LinkState::File => Some("a file".into()),
        _ => None,
    }
}

/// Both paths name the same file (after resolving links), or are equal when neither resolves.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// The shell line that puts `dir` on `PATH`, using `$HOME` when `dir` is inside it.
fn path_hint(dir: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let shown = match home.as_deref().and_then(|h| dir.strip_prefix(h).ok()) {
        Some(rest) => format!("$HOME/{}", rest.display()),
        None => dir.display().to_string(),
    };
    format!("export PATH=\"{}:$PATH\"", shown.replace(['"', '\\', '`'], ""))
}

/// `p` as one single-quoted `/bin/sh` word. Refuses non-UTF-8 and control characters.
pub fn sh_quote(p: &Path) -> Result<String> {
    let s = p.to_str().ok_or_else(|| CliToolError::Invalid(format!("{} is not valid UTF-8", p.display())))?;
    if s.is_empty() || s.chars().any(char::is_control) {
        return Err(CliToolError::Invalid(format!("{} can't be passed to the shell", p.display())));
    }
    Ok(format!("'{}'", s.replace('\'', r"'\''")))
}

/// Create `dir`, then point `link` at `target` by renaming a fresh link over it, so the name
/// never disappears and a crash leaves either the old entry or the new one.
#[cfg(unix)]
fn link_atomically(target: &Path, link: &Path) -> io::Result<()> {
    let dir = link.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the link has no folder"))?;
    std::fs::create_dir_all(dir)?;
    let name = link.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    std::fs::rename(&tmp, link).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(not(unix))]
fn link_atomically(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "command-line tool links need a Unix system"))
}

/// The macOS administrator prompt. The script and the prompt text travel as `osascript`
/// arguments (`item n of argv`), so neither is ever spliced into AppleScript source.
#[cfg(target_os = "macos")]
fn admin_prompt(script: &str, prompt: &str) -> Elevated {
    let out = std::process::Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "do shell script (item 1 of argv) with prompt (item 2 of argv) with administrator privileges",
            "-e",
            "end run",
            script,
            prompt,
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => Elevated::Done,
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            if err.contains("-128") { Elevated::Declined } else { Elevated::Failed(crate::os_text(err)) }
        }
        Err(e) => Elevated::Failed(crate::os_text(e)),
    }
}

#[cfg(not(target_os = "macos"))]
fn admin_prompt(_: &str, _: &str) -> Elevated {
    Elevated::Declined
}

// ---------------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------------

/// A command host that can hand out its app's [`CliTool`] (`None` in a development build with no
/// bundled tool: the commands are then disabled with a reason).
pub trait CliToolHost: Host<Error: From<CommandError>> {
    fn cli_tool(&self) -> Option<&CliTool>;
}

const INSTALL_PARAMS: &[Param] = &[Param::bool("replace").about("replace a file or link that isn't this app's tool")];

/// `app.installCommandLineTool` and `app.uninstallCommandLineTool` in `menu` (macOS: the app
/// menu, `&["<App name>"]`), and the menu-less query `app.commandLineToolStatus`. Each returns
/// the outcome as JSON with a `message` for a toast.
pub fn commands<S: CliToolHost>(menu: &'static [&'static str]) -> Vec<CommandSpec<S>> {
    vec![
        CommandSpec::new(INSTALL, "Install Command Line Tool…", run_install::<S>)
            .menu(menu)
            .params(Params::Static(INSTALL_PARAMS))
            .enabled(can_install::<S>)
            .query(),
        CommandSpec::new(UNINSTALL, "Uninstall Command Line Tool", run_uninstall::<S>).menu(menu).enabled(has_tool::<S>).query(),
        CommandSpec::new(STATUS, "Command Line Tool Status", run_status::<S>).enabled(has_tool::<S>).query(),
    ]
}

fn tool<S: CliToolHost>(s: &S) -> std::result::Result<&CliTool, S::Error> {
    s.cli_tool().ok_or_else(|| CommandError::failed("", "this build of the app has no command-line tool").into())
}

fn has_tool<S: CliToolHost>(s: &S) -> std::result::Result<(), String> {
    match s.cli_tool() {
        Some(_) => Ok(()),
        None => Err("this build of the app has no command-line tool".into()),
    }
}

fn can_install<S: CliToolHost>(s: &S) -> std::result::Result<(), String> {
    has_tool(s)?;
    match s.cli_tool() {
        Some(t) if t.helper_present() => Ok(()),
        _ => Err("the command-line tool is missing from this copy of the app".into()),
    }
}

fn to_value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn with_message(mut v: Value, message: String) -> Value {
    if let Some(m) = v.as_object_mut() {
        m.insert("message".into(), Value::String(message));
    }
    v
}

fn run_install<S: CliToolHost>(s: &mut S, p: &Value) -> std::result::Result<Value, S::Error> {
    let t = tool(s)?;
    let replace = p.get("replace").and_then(Value::as_bool).unwrap_or(false);
    let out = t.install(replace).map_err(|e| S::Error::from(CommandError::from(e)))?;
    Ok(with_message(to_value(&out), out.message(t.name())))
}

fn run_uninstall<S: CliToolHost>(s: &mut S, _: &Value) -> std::result::Result<Value, S::Error> {
    let t = tool(s)?;
    let out = t.uninstall().map_err(|e| S::Error::from(CommandError::from(e)))?;
    let kept: Vec<Value> = out.kept.iter().map(|(p, why)| json!({"path": p, "reason": why})).collect();
    Ok(json!({"removed": out.removed, "kept": kept, "message": out.message(t.name())}))
}

fn run_status<S: CliToolHost>(s: &mut S, _: &Value) -> std::result::Result<Value, S::Error> {
    let t = tool(s)?;
    let st = t.status();
    Ok(with_message(to_value(&st), st.message(t.name())))
}

#[cfg(test)]
#[cfg(unix)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::Mutex;

    use ashlar_command::{Journal, Registry};

    use super::*;

    struct Fixture {
        _root: tempfile::TempDir,
        app: PathBuf,
        helper: PathBuf,
        bin: PathBuf,
        fallback: PathBuf,
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("Some App.app");
        let helper = app.join("Contents/Helpers/tool");
        std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
        std::fs::write(&helper, "#!/bin/sh\necho tool 1.0\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let bin = root.path().join("usr local/bin");
        let fallback = root.path().join("home/.local/bin");
        Fixture { app, helper, bin, fallback, _root: root }
    }

    fn tool_at(f: &Fixture, elevation: Elevation) -> CliTool {
        CliTool::new("tool", &f.helper).unwrap().bin_dir(&f.bin).fallback_dir(Some(f.fallback.clone())).elevation(elevation)
    }

    fn read_only(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    }

    fn writable(dir: &Path) {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Root ignores permissions, so the "not writable" cases can't be staged.
    fn running_as_root() -> bool {
        let d = tempfile::tempdir().unwrap();
        read_only(d.path());
        let can = std::fs::write(d.path().join("probe"), "").is_ok();
        writable(d.path());
        can
    }

    #[test]
    fn names_are_checked() {
        for ok in ["tool", "my-tool", "a.b_c2"] {
            assert!(check_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-x", ".x", "a/b", "A", "a b", "a;b", &"x".repeat(65)] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn quoting_survives_quotes_and_spaces_and_refuses_control_characters() {
        let p = Path::new("/tmp/it's a \"dir\"/$(x)`y`");
        let q = sh_quote(p).unwrap();
        let out = std::process::Command::new("/bin/sh").arg("-c").arg(format!("printf %s {q}")).output().unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), p.to_str().unwrap());
        assert!(sh_quote(Path::new("/tmp/a\nb")).is_err());
        assert!(sh_quote(Path::new("")).is_err());
    }

    #[test]
    fn installs_into_a_missing_folder_then_reports_installed_and_is_idempotent() {
        let f = fixture();
        let t = tool_at(&f, Elevation::Never);
        assert_eq!(t.status().state, LinkState::Missing);
        let out = t.install(false).unwrap();
        assert_eq!(out.link, f.bin.join("tool"));
        assert!(!out.fallback && !out.unchanged && !out.elevated && out.path_hint.is_none());
        assert_eq!(std::fs::read_link(f.bin.join("tool")).unwrap(), f.helper);
        assert!(t.status().is_installed());
        let again = t.install(false).unwrap();
        assert!(again.unchanged);
        let leftovers: Vec<_> = std::fs::read_dir(&f.bin).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("tool")], "no temporary links left behind");
    }

    #[test]
    fn a_missing_helper_is_an_error_and_nothing_is_created() {
        let f = fixture();
        std::fs::remove_file(&f.helper).unwrap();
        let t = tool_at(&f, Elevation::Never);
        assert!(matches!(t.install(false), Err(CliToolError::HelperMissing(_))));
        assert!(!f.bin.exists());
    }

    #[test]
    fn never_replaces_what_isnt_ours_without_replace() {
        let f = fixture();
        std::fs::create_dir_all(&f.bin).unwrap();
        let link = f.bin.join("tool");
        std::fs::write(&link, "someone else's").unwrap();
        let t = tool_at(&f, Elevation::Never);
        assert_eq!(t.status().state, LinkState::File);
        let err = t.install(false).unwrap_err();
        assert!(err.is_conflict(), "{err}");
        assert!(err.to_string().contains("replace"), "{err}");
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "someone else's");
        let out = t.install(true).unwrap();
        assert_eq!(out.replaced.as_deref(), Some("a file"));
        assert!(out.message("tool").contains("replaced a file"));
        assert!(t.status().is_installed());
    }

    #[test]
    fn a_link_to_something_else_is_foreign_and_is_kept_on_uninstall() {
        let f = fixture();
        std::fs::create_dir_all(&f.bin).unwrap();
        let link = f.bin.join("tool");
        symlink("/bin/echo", &link).unwrap();
        let t = tool_at(&f, Elevation::Never);
        assert_eq!(t.status().state, LinkState::ForeignLink { target: "/bin/echo".into() });
        assert!(t.install(false).unwrap_err().is_conflict());
        let r = t.uninstall().unwrap();
        assert!(r.removed.is_empty());
        assert_eq!(r.kept.len(), 1);
        assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("/bin/echo"));
    }

    #[test]
    fn a_folder_is_never_replaced() {
        let f = fixture();
        std::fs::create_dir_all(f.bin.join("tool")).unwrap();
        let t = tool_at(&f, Elevation::Never);
        let err = t.install(true).unwrap_err();
        assert!(matches!(err, CliToolError::Conflict { replaceable: false, .. }), "{err}");
    }

    #[test]
    fn a_link_left_by_a_moved_app_is_broken_and_repaired_without_asking() {
        let f = fixture();
        std::fs::create_dir_all(&f.bin).unwrap();
        let old = f.app.parent().unwrap().join("Old Place/Some App.app/Contents/Helpers/tool");
        symlink(&old, f.bin.join("tool")).unwrap();
        let t = tool_at(&f, Elevation::Never);
        let st = t.status();
        assert!(st.needs_repair(), "{st:?}");
        assert!(st.message("tool").contains("repair"));
        let out = t.repair().unwrap().unwrap();
        assert!(out.repaired && out.replaced.is_none());
        assert!(t.status().is_installed());
        assert!(t.repair().unwrap().is_none(), "nothing left to repair");
    }

    #[test]
    fn a_link_to_another_copy_needs_replace() {
        let f = fixture();
        let other = f.app.parent().unwrap().join("Copy.app/Contents/Helpers/tool");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, "").unwrap();
        std::fs::create_dir_all(&f.bin).unwrap();
        symlink(&other, f.bin.join("tool")).unwrap();
        let t = tool_at(&f, Elevation::Never);
        assert_eq!(t.status().state, LinkState::OtherCopy { target: other.clone() });
        assert!(t.install(false).unwrap_err().is_conflict());
        let r = t.uninstall().unwrap();
        assert!(r.removed.is_empty() && r.kept.len() == 1);
        assert!(t.install(true).unwrap().replaced.unwrap().contains("Copy.app"));
    }

    #[test]
    fn a_read_only_folder_asks_for_admin_and_uses_the_answer() {
        if running_as_root() {
            return;
        }
        let f = fixture();
        read_only(&f.bin);
        let scripts: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = scripts.clone();
        let bin = f.bin.clone();
        // "Administrator": make the folder writable, then run the script as given.
        let accept: Elevator = Arc::new(move |script: &str| {
            seen.lock().unwrap().push(script.to_string());
            writable(&bin);
            let ok = std::process::Command::new("/bin/sh").arg("-c").arg(script).status().unwrap().success();
            if ok { Elevated::Done } else { Elevated::Failed("script failed".into()) }
        });
        let t = tool_at(&f, Elevation::Custom(accept));
        let out = t.install(false).unwrap();
        assert!(out.elevated && !out.fallback);
        assert!(t.status().is_installed());
        let script = scripts.lock().unwrap().first().cloned().unwrap();
        assert!(script.starts_with("/bin/mkdir -p '") && script.contains("/bin/ln -sfn '"), "{script}");
        assert!(script.contains("'") && script.contains("usr local/bin"), "paths are quoted: {script}");
        // Uninstall from a read-only folder asks again.
        read_only(&f.bin);
        let r = t.uninstall().unwrap();
        assert_eq!(r.removed, vec![f.bin.join("tool")]);
        assert!(scripts.lock().unwrap().last().unwrap().starts_with("/bin/rm -f '"));
        writable(&f.bin);
    }

    #[test]
    fn a_declined_prompt_falls_back_to_the_user_folder_with_a_path_hint() {
        if running_as_root() {
            return;
        }
        let f = fixture();
        read_only(&f.bin);
        let asked = Arc::new(Mutex::new(0));
        let n = asked.clone();
        let decline: Elevator = Arc::new(move |_: &str| {
            *n.lock().unwrap() += 1;
            Elevated::Declined
        });
        let t = tool_at(&f, Elevation::Custom(decline));
        let out = t.install(false).unwrap();
        assert_eq!(*asked.lock().unwrap(), 1);
        assert!(out.fallback && !out.elevated);
        assert_eq!(out.link, f.fallback.join("tool"));
        let hint = out.path_hint.clone().unwrap();
        assert!(hint.starts_with("export PATH=\"") && hint.ends_with(":$PATH\""), "{hint}");
        assert!(out.message("tool").contains("~/.zshrc"));
        let st = t.status();
        assert!(st.fallback && st.is_installed(), "{st:?}");
        // Uninstall removes the fallback link without asking (the system folder is empty).
        let r = t.uninstall().unwrap();
        assert_eq!(r.removed, vec![f.fallback.join("tool")]);
        assert_eq!(*asked.lock().unwrap(), 1);
        writable(&f.bin);
    }

    #[test]
    fn without_a_fallback_a_declined_prompt_is_an_error() {
        if running_as_root() {
            return;
        }
        let f = fixture();
        read_only(&f.bin);
        let t = tool_at(&f, Elevation::Never).fallback_dir(None);
        assert!(matches!(t.install(false), Err(CliToolError::Os(_))));
        writable(&f.bin);
    }

    #[test]
    fn a_failed_admin_script_is_reported_not_hidden() {
        if running_as_root() {
            return;
        }
        let f = fixture();
        read_only(&f.bin);
        let fail: Elevator = Arc::new(|_: &str| Elevated::Failed("the disk said no".into()));
        let t = tool_at(&f, Elevation::Custom(fail));
        assert_eq!(t.install(false).unwrap_err(), CliToolError::Os("the disk said no".into()));
        assert!(!f.fallback.join("tool").exists());
        writable(&f.bin);
    }

    #[test]
    fn without_env_overrides_the_system_folder_and_the_prompt_are_used() {
        let f = fixture();
        let t = CliTool::new("tool", &f.helper).unwrap();
        if std::env::var_os(ENV_BIN_DIR).is_none() && std::env::var_os(ENV_FALLBACK_DIR).is_none() {
            assert_eq!(t.link(), Path::new(SYSTEM_BIN_DIR).join("tool"));
            assert!(matches!(t.elevation, Elevation::Prompt));
        } else {
            assert!(matches!(t.elevation, Elevation::Never));
        }
    }

    #[test]
    fn bundled_helper_is_next_to_the_executables_folder() {
        let p = bundled_helper("tool").unwrap();
        assert!(p.ends_with("Helpers/tool"), "{}", p.display());
        assert!(bundled_helper("../x").is_err());
    }

    // --- commands ---

    struct App {
        journal: Journal,
        tool: Option<CliTool>,
    }

    impl Host for App {
        type Error = CommandError;
        fn journal(&mut self) -> &mut Journal {
            &mut self.journal
        }
    }

    impl CliToolHost for App {
        fn cli_tool(&self) -> Option<&CliTool> {
            self.tool.as_ref()
        }
    }

    #[test]
    fn commands_install_report_and_uninstall() {
        let f = fixture();
        let reg = Registry::new(commands::<App>(&["App"]));
        let mut app = App { journal: Journal::default(), tool: Some(tool_at(&f, Elevation::Never)) };
        let ids: Vec<_> = reg.specs().iter().map(|s| s.id).collect();
        assert_eq!(ids, [INSTALL, UNINSTALL, STATUS]);
        assert_eq!(reg.find(INSTALL).unwrap().menu, &["App"]);
        assert!(reg.find(STATUS).unwrap().menu.is_empty());

        let st = reg.execute(&mut app, STATUS, json!({})).unwrap();
        assert_eq!(st["state"], "missing");
        let out = reg.execute(&mut app, INSTALL, json!({})).unwrap();
        assert!(out["message"].as_str().unwrap().starts_with("Installed `tool`"), "{out}");
        assert_eq!(reg.execute(&mut app, STATUS, json!({})).unwrap()["state"], "ours");
        let out = reg.execute(&mut app, UNINSTALL, json!({})).unwrap();
        assert_eq!(out["removed"].as_array().unwrap().len(), 1, "{out}");
        assert!(!f.bin.join("tool").exists());
    }

    #[test]
    fn commands_refuse_a_conflict_with_a_reason_and_accept_replace() {
        let f = fixture();
        std::fs::create_dir_all(&f.bin).unwrap();
        std::fs::write(f.bin.join("tool"), "x").unwrap();
        let reg = Registry::new(commands::<App>(&["App"]));
        let mut app = App { journal: Journal::default(), tool: Some(tool_at(&f, Elevation::Never)) };
        let err = reg.execute(&mut app, INSTALL, json!({})).unwrap_err();
        assert!(err.to_string().contains("nothing was changed"), "{err}");
        let out = reg.execute(&mut app, INSTALL, json!({"replace": true})).unwrap();
        assert_eq!(out["replaced"], "a file");
    }

    #[test]
    fn commands_are_disabled_without_a_tool_or_helper() {
        let f = fixture();
        let reg = Registry::new(commands::<App>(&["App"]));
        let mut app = App { journal: Journal::default(), tool: None };
        assert!(reg.execute(&mut app, INSTALL, json!({})).is_err());
        let info = reg.info(reg.find(INSTALL).unwrap(), &app);
        assert!(!info.enabled);
        std::fs::remove_file(&f.helper).unwrap();
        app.tool = Some(tool_at(&f, Elevation::Never));
        let info = reg.info(reg.find(INSTALL).unwrap(), &app);
        assert!(!info.enabled && info.disabled_reason.unwrap().contains("missing"));
        assert!(reg.info(reg.find(UNINSTALL).unwrap(), &app).enabled);
    }
}
