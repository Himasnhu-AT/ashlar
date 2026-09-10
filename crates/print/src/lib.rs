#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! Hand an already composed PDF to the operating system's print spooler.
//!
//! - On Unix the job goes to CUPS: printers come from `lpstat`, driver options from the queue's
//!   PPD file and `lpoptions`, and the PDF is streamed to `lp` on its standard input.
//! - On Windows an inbox PowerShell script renders each page with the system PDF renderer and
//!   prints it through System.Drawing, picking paper and orientation per page.
//! - Elsewhere discovery is empty and [`submit`] returns an [`Error`].
//!
//! Everything here is synchronous and may take a while: call it from a worker thread. This crate
//! makes no PDFs and draws no UI.

use std::fmt;
use std::process::Command;

#[cfg(test)]
mod tests;

/// A spooler failure, with a message meant for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Spool(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Spool(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

/// A print queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Printer {
    pub name: String,
    /// The system's default destination.
    pub default: bool,
    /// Whether the driver reports duplex support: known on Windows, `None` on CUPS.
    pub two_sided: Option<bool>,
}

/// Two-sided printing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Duplex {
    #[default]
    Off,
    /// Flip on the long edge (portrait books).
    LongEdge,
    /// Flip on the short edge (calendars, landscape).
    ShortEdge,
}

/// How a page meets the paper.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Scaling {
    /// Physical size, unscaled; hardware margins clip.
    #[default]
    Exact,
    /// Scaled down (never up) into the printable area.
    ShrinkToFit,
}

/// One print job.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    /// Queue name; `None` for the system default.
    pub printer: Option<String>,
    pub copies: u32,
    pub collate: bool,
    /// `None` keeps the queue's own default.
    pub duplex: Option<Duplex>,
    pub scaling: Scaling,
    pub grayscale: bool,
    /// Name shown in the queue.
    pub title: String,
    /// Driver options as (keyword, choice) pairs, from [`printer_options`]. CUPS only.
    pub options: Vec<(String, String)>,
}

impl Default for Job {
    fn default() -> Self {
        Self {
            printer: None,
            copies: 1,
            collate: true,
            duplex: None,
            scaling: Scaling::Exact,
            grayscale: false,
            title: "ashlar".to_string(),
            options: Vec::new(),
        }
    }
}

/// A driver option a print dialog can offer (from a CUPS PPD).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrinterOption {
    pub key: String,
    pub label: String,
    /// Label of the PPD group it was declared in (empty outside any group).
    pub group: String,
    /// (keyword, label) pairs.
    pub choices: Vec<(String, String)>,
    /// The keyword currently selected.
    pub default: String,
}

/// PPD options this crate sets itself (or that only describe the paper); [`parse_ppd`] skips
/// them and [`lp_args`] never forwards them.
pub const HANDLED_OPTIONS: &[&str] = &["PageSize", "PageRegion", "Duplex", "Collate", "ImageableArea", "PaperDimension"];

/// Whether [`open_printer_preferences`] can open a driver dialog on this platform.
pub const HAS_PRINTER_PREFERENCES: bool = cfg!(windows);

/// Which spooler [`submit`] talks to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Cups,
    WindowsPdf,
}

/// Most choices kept per option and options kept per PPD.
const PPD_LIMIT: usize = 400;
/// Largest PPD file read.
#[cfg(all(unix, not(target_arch = "wasm32")))]
const PPD_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// Highest copy count passed to `lp`.
const MAX_COPIES: u32 = 999;

fn unavailable() -> Error {
    Error::Spool("Printing is not available on this platform. Save the document as a PDF and print it from another app.".to_string())
}

// ---------------------------------------------------------------------------------------------
// CUPS

fn default_destination(lpstat: &str) -> Option<&str> {
    lpstat.lines().find_map(|line| line.strip_prefix("system default destination:")).map(str::trim)
}

/// Queues listed by `lpstat -p -d` (run in the C locale; localized output yields nothing).
pub fn parse_lpstat(out: &str) -> Vec<Printer> {
    let default = default_destination(out);
    out.lines()
        .filter_map(|line| line.strip_prefix("printer ")?.split_whitespace().next())
        .map(|name| Printer { name: name.to_string(), default: Some(name) == default, two_sided: None })
        .collect()
}

/// Destinations listed by `lpstat -e`, one per line.
pub fn parse_lpstat_e(out: &str) -> Vec<String> {
    out.lines().map(str::trim).filter(|line| !line.is_empty()).map(str::to_string).collect()
}

/// Combine `lpstat -p -d` output with `lpstat -e` output: the queues first, then destinations
/// that only `-e` knows (network printers that have no local queue yet).
pub fn printers_parsed(queues: &str, available: Option<&str>) -> Vec<Printer> {
    let mut list = parse_lpstat(queues);
    let Some(available) = available else { return list };
    let default = default_destination(queues);
    let mut seen: std::collections::HashSet<String> = list.iter().map(|p| p.name.clone()).collect();
    for name in parse_lpstat_e(available) {
        if seen.insert(name.clone()) {
            list.push(Printer { default: Some(name.as_str()) == default, name, two_sided: None });
        }
    }
    list
}

/// Fixes the environment of a CUPS tool so its output is not localized.
fn c_locale(mut command: Command) -> Command {
    // macOS CUPS ignores LANG unless SOFTWARE is set.
    command.env("LC_ALL", "C").env("LANG", "C").env("SOFTWARE", "ashlar-print");
    command
}

/// The `lpstat -p -d` command [`printers`] runs, in the C locale.
pub fn lpstat_command() -> Command {
    let mut command = Command::new("lpstat");
    command.args(["-p", "-d"]);
    c_locale(command)
}

#[cfg(all(unix, not(target_arch = "wasm32")))]
fn lpstat_destinations_command() -> Command {
    let mut command = Command::new("lpstat");
    command.arg("-e");
    c_locale(command)
}

/// The printers this machine can print to. Empty when the spooler cannot be asked.
pub fn printers() -> Vec<Printer> {
    #[cfg(all(unix, not(target_arch = "wasm32")))]
    {
        let Ok(queues) = lpstat_command().output() else { return Vec::new() };
        let available = lpstat_destinations_command().output().ok().map(|out| String::from_utf8_lossy(&out.stdout).into_owned());
        printers_parsed(&String::from_utf8_lossy(&queues.stdout), available.as_deref())
    }
    #[cfg(windows)]
    {
        match powershell_command(LIST_PRINTERS_SCRIPT).output() {
            Ok(out) if out.status.success() => parse_windows_printers(&String::from_utf8_lossy(&out.stdout)),
            _ => Vec::new(),
        }
    }
    #[cfg(not(any(windows, all(unix, not(target_arch = "wasm32")))))]
    {
        Vec::new()
    }
}

/// A PPD keyword: 1–64 printable ASCII bytes without spaces or `/ : = "`. Anything else is not
/// passed to `lp` or used as a file name.
fn keyword(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'/' | b':' | b'=' | b'"'))
}

/// `name/label` → (name, label); the label falls back to the name.
fn name_and_label(s: &str) -> (String, String) {
    match s.split_once('/') {
        Some((name, label)) => {
            let (name, label) = (name.trim(), label.trim());
            (name.to_string(), if label.is_empty() { name } else { label }.to_string())
        }
        None => (s.trim().to_string(), s.trim().to_string()),
    }
}

/// Arguments for `lp` (the document itself goes on its standard input).
pub fn lp_args(job: &Job) -> Vec<String> {
    let mut args = Vec::new();
    let option = |args: &mut Vec<String>, value: String| {
        args.push("-o".to_string());
        args.push(value);
    };
    if let Some(printer) = &job.printer {
        args.extend(["-d".to_string(), printer.clone()]);
    }
    args.extend(["-n".to_string(), job.copies.clamp(1, MAX_COPIES).to_string()]);
    args.extend(["-t".to_string(), job.title.clone()]);
    option(&mut args, format!("collate={}", job.collate));
    if let Some(duplex) = job.duplex {
        let sides = match duplex {
            Duplex::Off => "one-sided",
            Duplex::LongEdge => "two-sided-long-edge",
            Duplex::ShortEdge => "two-sided-short-edge",
        };
        option(&mut args, format!("sides={sides}"));
    }
    if job.grayscale {
        option(&mut args, "print-color-mode=monochrome".to_string());
    }
    match job.scaling {
        Scaling::Exact => option(&mut args, "fit-to-page=false".to_string()),
        Scaling::ShrinkToFit => option(&mut args, "print-scaling=auto-fit".to_string()),
    }
    for (key, value) in &job.options {
        if keyword(key) && keyword(value) && !HANDLED_OPTIONS.contains(&key.as_str()) {
            option(&mut args, format!("{key}={value}"));
        }
    }
    args
}

/// The user-selectable options of a PPD file. Lenient: unknown or malformed lines are skipped.
pub fn parse_ppd(ppd: &str) -> Vec<PrinterOption> {
    let mut kept: Vec<PrinterOption> = Vec::new();
    let mut group: Option<(String, String)> = None;
    let mut open: Option<PrinterOption> = None;
    for line in ppd.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix("*OpenGroup:") {
            group = Some(name_and_label(rest));
        } else if line.starts_with("*CloseGroup") {
            group = None;
        } else if let Some(rest) = line.strip_prefix("*OpenUI") {
            open = None;
            let Some((head, _)) = rest.rsplit_once(':') else { continue };
            let head = head.trim();
            let (key, label) = name_and_label(head.strip_prefix('*').unwrap_or(head));
            if keyword(&key) {
                let group = group.as_ref().map(|(_, label)| label.clone()).unwrap_or_default();
                open = Some(PrinterOption { key, label, group, choices: Vec::new(), default: String::new() });
            }
        } else if line.starts_with("*CloseUI") {
            let Some(option) = open.take() else { continue };
            let installable = group.as_ref().is_some_and(|(key, _)| key == "InstallableOptions");
            if !option.choices.is_empty() && !installable && !HANDLED_OPTIONS.contains(&option.key.as_str()) && kept.len() < PPD_LIMIT {
                kept.push(option);
            }
        } else if let Some(option) = open.as_mut() {
            read_option_line(option, line);
        }
    }
    for option in &mut kept {
        if !option.choices.iter().any(|(choice, _)| *choice == option.default)
            && let Some((first, _)) = option.choices.first()
        {
            option.default = first.clone();
        }
    }
    kept
}

/// A `*Default<key>:` or `*<key> <choice>/<label>: …` line inside an open option.
fn read_option_line(option: &mut PrinterOption, line: &str) {
    let Some(rest) = line.strip_prefix('*') else { return };
    if let Some(value) = rest.strip_prefix("Default").and_then(|r| r.strip_prefix(option.key.as_str())).and_then(|r| r.strip_prefix(':')) {
        option.default = value.trim().to_string();
        return;
    }
    let Some(after_key) = rest.strip_prefix(option.key.as_str()) else { return };
    if !after_key.starts_with([' ', '\t']) {
        return;
    }
    let head = after_key.split(':').next().unwrap_or_default();
    let (choice, label) = name_and_label(head);
    if keyword(&choice) && option.choices.len() < PPD_LIMIT && !option.choices.iter().any(|(c, _)| *c == choice) {
        option.choices.push((choice, label));
    }
}

/// The current choices from `lpoptions -p <printer> -l`: lines `Key/Label: a *b c` give
/// `(Key, b)`.
pub fn parse_lpoptions(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|line| {
            let (head, choices) = line.split_once(':')?;
            let key = head.split('/').next()?.trim();
            let current = choices.split_whitespace().find_map(|word| word.strip_prefix('*'))?;
            (!key.is_empty() && !current.is_empty()).then(|| (key.to_string(), current.to_string()))
        })
        .collect()
}

/// PPD bytes as text: UTF-8 when valid, otherwise Latin-1 (the PPD default encoding).
pub fn ppd_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
    }
}

/// The driver options of a CUPS queue, with the queue's current choices as defaults. Reads a
/// file and runs `lpoptions`: call it when a print dialog opens, not every frame. Empty on other
/// platforms, for names that are not plain keywords, and when the PPD cannot be read.
pub fn printer_options(printer: &str) -> Vec<PrinterOption> {
    #[cfg(all(unix, not(target_arch = "wasm32")))]
    {
        use std::io::Read;
        // A keyword has no '/', so the name cannot leave the PPD directory.
        if !keyword(printer) {
            return Vec::new();
        }
        let path = std::path::Path::new("/etc/cups/ppd").join(format!("{printer}.ppd"));
        let mut bytes = Vec::new();
        let read = std::fs::File::open(&path).and_then(|file| file.take(PPD_MAX_BYTES + 1).read_to_end(&mut bytes));
        if read.is_err() || bytes.len() as u64 > PPD_MAX_BYTES {
            return Vec::new();
        }
        let mut options = parse_ppd(&ppd_text(&bytes));
        let mut lpoptions = c_locale(Command::new("lpoptions"));
        lpoptions.args(["-p", printer, "-l"]);
        if let Ok(out) = lpoptions.output() {
            for (key, value) in parse_lpoptions(&String::from_utf8_lossy(&out.stdout)) {
                if let Some(option) = options.iter_mut().find(|o| o.key == key)
                    && option.choices.iter().any(|(choice, _)| *choice == value)
                {
                    option.default = value;
                }
            }
        }
        options
    }
    #[cfg(not(all(unix, not(target_arch = "wasm32"))))]
    {
        let _ = printer;
        Vec::new()
    }
}

/// Open the driver's own preferences dialog for `printer` (Windows only; see
/// [`HAS_PRINTER_PREFERENCES`]). Returns once the dialog process has started.
pub fn open_printer_preferences(printer: &str) -> Result<(), Error> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if printer.is_empty() || printer.contains('"') {
            return Err(Error::Spool(format!("\"{printer}\" is not a printer name the preferences dialog can open.")));
        }
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let rundll = std::path::PathBuf::from(root).join("System32").join("rundll32.exe");
        let mut command = Command::new(rundll);
        command.raw_arg("printui.dll,PrintUIEntry").raw_arg("/e").raw_arg("/n").raw_arg(format!("\"{printer}\""));
        command.spawn().map(drop).map_err(|e| Error::Spool(format!("The printer preferences could not be opened: {e}")))
    }
    #[cfg(not(windows))]
    {
        let _ = printer;
        Err(Error::Spool("A driver preferences dialog only exists on Windows; use the printer options instead.".to_string()))
    }
}

/// Send `pdf` to the spooler. Returns the spooler's receipt (for CUPS, the `lp` job line).
pub fn submit(pdf: &[u8], job: &Job) -> Result<String, Error> {
    #[cfg(all(unix, not(target_arch = "wasm32")))]
    {
        submit_via(Command::new("lp"), pdf, job)
    }
    #[cfg(windows)]
    {
        windows_submit(pdf, job, &[])
    }
    #[cfg(not(any(windows, all(unix, not(target_arch = "wasm32")))))]
    {
        let _ = (pdf, job);
        Err(unavailable())
    }
}

/// Run `lp` (or a stand-in) with the job's arguments and stream `pdf` to its standard input.
#[cfg(any(test, all(unix, not(target_arch = "wasm32"))))]
// Only the Unix tests drive it with a stand-in elsewhere.
#[cfg_attr(not(all(unix, not(target_arch = "wasm32"))), allow(dead_code))]
pub(crate) fn submit_via(mut lp: Command, pdf: &[u8], job: &Job) -> Result<String, Error> {
    use std::io::Write;
    use std::process::Stdio;

    lp.args(lp_args(job)).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = lp.spawn().map_err(|e| Error::Spool(format!("The print spooler is not available: {e}")))?;
    let stdin = child.stdin.take();
    // The document is written on its own thread while this one collects the output, so a
    // spooler that answers before reading everything cannot deadlock either side.
    let (output, sent) = std::thread::scope(|scope| {
        let writer = std::thread::Builder::new().name("ashlar-print-stdin".to_string()).spawn_scoped(scope, move || match stdin {
            Some(mut stdin) => stdin.write_all(pdf).and_then(|()| stdin.flush()),
            None => Err(std::io::Error::other("the spooler has no standard input")),
        });
        let writer = match writer {
            Ok(writer) => writer,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return (Err(e), None);
            }
        };
        let output = child.wait_with_output();
        (output, Some(writer.join()))
    });
    let output = output.map_err(|e| Error::Spool(format!("The print spooler could not be run: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(Error::Spool(if stderr.is_empty() { "The print spooler refused the job.".to_string() } else { stderr.to_string() }));
    }
    match sent {
        Some(Ok(Ok(()))) => Ok(job_message(&output.stdout)),
        Some(Ok(Err(e))) => Err(Error::Spool(format!("The document could not be sent to the print spooler: {e}"))),
        Some(Err(_)) | None => Err(Error::Spool("The document could not be sent to the print spooler.".to_string())),
    }
}

/// The receipt from `lp`'s output, without the `(0 file(s))` CUPS appends for standard input.
#[cfg(any(test, all(unix, not(target_arch = "wasm32"))))]
pub(crate) fn job_message(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout);
    let text = text.trim();
    text.strip_suffix("(0 file(s))").map(str::trim_end).unwrap_or(text).to_string()
}

/// The spooler [`submit`] uses on this platform.
pub const fn backend() -> Option<Backend> {
    if cfg!(windows) {
        Some(Backend::WindowsPdf)
    } else if cfg!(all(unix, not(target_arch = "wasm32"))) {
        Some(Backend::Cups)
    } else {
        None
    }
}

/// The command line [`submit`] would run, for diagnostics. Starts nothing and writes nothing.
pub fn command_preview(job: &Job) -> Result<Vec<String>, Error> {
    match backend() {
        Some(Backend::Cups) => Ok(std::iter::once("lp".to_string()).chain(lp_args(job)).collect()),
        Some(Backend::WindowsPdf) => {
            Ok(["powershell", "-NoProfile", "-NonInteractive", "-Command", "<embedded windows_print.ps1>"].into_iter().map(str::to_string).collect())
        }
        None => Err(unavailable()),
    }
}

// ---------------------------------------------------------------------------------------------
// Windows

/// Lists printers as `[*]name<TAB>1|2` (default marker, duplex capability).
#[cfg(windows)]
const LIST_PRINTERS_SCRIPT: &str = "[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false; \
Get-CimInstance -ClassName Win32_Printer -Property Name,Default,Capabilities | ForEach-Object { \
$mark = if ($_.Default) { '*' } else { '' }; \
$sides = if ($_.Capabilities -contains 3) { '2' } else { '1' }; \
[Console]::Out.WriteLine($mark + $_.Name + \"`t\" + $sides) }";

/// The page renderer and print loop run by [`windows_submit`].
#[cfg(windows)]
pub(crate) const WINDOWS_PRINT_SCRIPT: &str = include_str!("windows_print.ps1");

/// Printers from the listing script's output.
pub fn parse_windows_printers(out: &str) -> Vec<Printer> {
    let mut printers: Vec<Printer> = Vec::new();
    let mut seen_default = false;
    for line in out.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        let (rest, two_sided) = match line.rsplit_once('\t') {
            Some((name, "2")) => (name, Some(true)),
            Some((name, "1")) => (name, Some(false)),
            _ => (line, None),
        };
        let (name, default) = match rest.strip_prefix('*') {
            Some(name) => (name, true),
            None => (rest, false),
        };
        let default = default && !seen_default;
        seen_default |= default;
        printers.push(Printer { name: name.to_string(), default, two_sided });
    }
    printers
}

/// The environment the print script reads its job from. Nothing goes on the command line, so
/// names with quotes survive and the document path does not show up in process lists.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn windows_env_vars(path: &std::path::Path, job: &Job) -> Vec<(String, String)> {
    let flag = |on: bool| if on { "1" } else { "0" }.to_string();
    let duplex = match job.duplex {
        None => "",
        Some(Duplex::Off) => "off",
        Some(Duplex::LongEdge) => "long-edge",
        Some(Duplex::ShortEdge) => "short-edge",
    };
    let scaling = match job.scaling {
        Scaling::Exact => "exact",
        Scaling::ShrinkToFit => "shrink-to-fit",
    };
    [
        ("ASHLAR_PRINT_FILE", path.to_string_lossy().into_owned()),
        ("ASHLAR_PRINT_PRINTER", job.printer.clone().unwrap_or_default()),
        ("ASHLAR_PRINT_COPIES", job.copies.to_string()),
        ("ASHLAR_PRINT_COLLATE", flag(job.collate)),
        ("ASHLAR_PRINT_DUPLEX", duplex.to_string()),
        ("ASHLAR_PRINT_SCALING", scaling.to_string()),
        ("ASHLAR_PRINT_GRAYSCALE", flag(job.grayscale)),
        ("ASHLAR_PRINT_TITLE", job.title.clone()),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value))
    .collect()
}

/// Write the document to a fresh, randomly named file in the temp directory. The file is removed
/// when the returned path is dropped.
#[cfg(windows)]
pub(crate) fn windows_job_file(pdf: &[u8]) -> Result<tempfile::TempPath, Error> {
    use std::io::Write;
    let failed = |e: std::io::Error| Error::Spool(format!("The print job could not be written: {e}"));
    let mut file = tempfile::Builder::new().prefix("ashlar-print-").suffix(".pdf").tempfile().map_err(failed)?;
    file.write_all(pdf).and_then(|()| file.flush()).map_err(failed)?;
    Ok(file.into_temp_path())
}

/// Inbox Windows PowerShell, by absolute path, with the test-only switches removed.
#[cfg(windows)]
pub(crate) fn powershell_command(script: &str) -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let exe = std::path::PathBuf::from(root).join("System32").join("WindowsPowerShell").join("v1.0").join("powershell.exe");
    let mut command = Command::new(exe);
    command.args(["-NoProfile", "-NonInteractive", "-Command", script]).creation_flags(CREATE_NO_WINDOW);
    command.env_remove("ASHLAR_PRINT_DRYRUN").env_remove("ASHLAR_PRINT_TEST_SETTINGS");
    command
}

/// Print `pdf` with the embedded script; `extra_envs` are added after the job's variables.
#[cfg(windows)]
pub(crate) fn windows_submit(pdf: &[u8], job: &Job, extra_envs: &[(&str, &str)]) -> Result<String, Error> {
    let file = windows_job_file(pdf)?;
    let mut command = powershell_command(WINDOWS_PRINT_SCRIPT);
    command.envs(windows_env_vars(&file, job));
    command.envs(extra_envs.iter().copied());
    let output = command.output();
    // The document is removed before anything else happens, whatever the outcome.
    drop(file);
    let output = output.map_err(|e| Error::Spool(format!("The print spooler is not available: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if output.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(Error::Spool(if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        "The print spooler refused the job.".to_string()
    }))
}
