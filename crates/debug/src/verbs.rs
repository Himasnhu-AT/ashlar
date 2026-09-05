//! The commands: each parses its arguments, makes one or two control calls, and formats the result.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ashlar_command::CommandInfo;
use ashlar_control::Client;
use base64::Engine;
use serde_json::{Value, json};

use crate::session::{self, Session, Trace};
use crate::words::{Args, quote};
use crate::{Ctx, Done, Fail, Res, target, usage_of};

/// What `get` reads (`ui.get {what}`).
const GET_WHAT: &[&str] = &["text", "value", "name", "role", "rect", "enabled", "visible", "focused", "checked", "selected", "element"];

/// Verbs that change the UI: a trace records after each.
const ACTIONS: &[&str] = &["click", "dblclick", "hover", "focus", "fill", "type", "press", "scroll", "scrollintoview", "drag", "exec", "menu"];

fn unix_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// Read a JSON argument: inline text, `-` for stdin, absent for `{}`.
fn json_arg(raw: Option<String>, input: &mut dyn std::io::BufRead) -> Result<Value, String> {
    let text = match raw.as_deref() {
        None => return Ok(json!({})),
        Some("-") => {
            let mut s = String::new();
            input.read_to_string(&mut s).map_err(|e| format!("cannot read stdin: {e}"))?;
            s
        }
        Some(t) => t.to_string(),
    };
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&text).map_err(|e| format!("not valid JSON: {e}"))
}

/// A value for humans: strings as they are, everything else as compact JSON.
fn plain(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The outline of a snapshot: the app's `tree`, or one built from `elements`.
pub(crate) fn outline(v: &Value) -> String {
    if let Some(t) = v.get("tree").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) {
        return t.trim_end().to_string();
    }
    let els = v.get("elements").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut lines = Vec::new();
    for e in &els {
        let s = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or("");
        let mut depth = 0usize;
        let mut parent = e.get("parent").and_then(Value::as_str).map(String::from);
        while let Some(p) = parent.take() {
            depth = depth.saturating_add(1);
            if depth > 64 {
                break;
            }
            parent = els
                .iter()
                .find(|x| x.get("ref").and_then(Value::as_str) == Some(&p))
                .and_then(|x| x.get("parent"))
                .and_then(Value::as_str)
                .map(String::from);
        }
        let mut line = format!("{}- {} \"{}\" [ref={}]", "  ".repeat(depth), s("role"), s("name"), s("ref"));
        if e.get("enabled") == Some(&json!(false)) {
            line.push_str(" [disabled]");
        }
        lines.push(line);
    }
    lines.join("\n")
}

fn need<T>(v: Option<T>, what: &str, verb: &str) -> Result<T, Fail> {
    v.ok_or_else(|| Fail::Usage(format!("{verb} needs {what}"), usage_of(verb)))
}

fn usage(verb: &str) -> impl Fn(String) -> Fail + '_ {
    move |e| Fail::Usage(e, usage_of(verb))
}

fn parse_target(s: &str, verb: &str) -> Result<Value, Fail> {
    target::parse(s).map_err(usage(verb))
}

/// `--mods Shift+Mod` as the protocol's modifier object.
fn modifiers(s: &str) -> Result<Value, String> {
    let mut m = serde_json::Map::new();
    for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        let key = match part.to_ascii_lowercase().as_str() {
            "mod" | "cmd" | "command" => "command",
            "shift" => "shift",
            "alt" | "option" | "opt" => "alt",
            "ctrl" | "control" => "ctrl",
            other => return Err(format!("unknown modifier `{other}` (Mod, Shift, Alt, Ctrl)")),
        };
        m.insert(key.into(), json!(true));
    }
    Ok(Value::Object(m))
}

impl Ctx<'_, '_, '_> {
    /// Run one verb with its arguments; in a traced session, record after an action. `shown` is
    /// the command as written (for the trace).
    pub(crate) fn run_verb(&mut self, verb: &str, a: Args, shown: &[String]) -> Res {
        let r = self.dispatch(verb, a);
        if ACTIONS.contains(&verb) {
            self.record(verb, shown, &r);
        }
        r
    }

    fn dispatch(&mut self, verb: &str, mut a: Args) -> Res {
        if a.flag(&["--help", "-h"]) {
            return Ok(Done::new(json!({"usage": usage_of(verb)}), format!("usage: {}", usage_of(verb))));
        }
        match verb {
            "open" => self.open(a),
            "connect" => self.connect(a),
            "close" => self.close(a),
            "status" => self.status(a),
            "sessions" => self.sessions(a),
            "snapshot" => self.snapshot(a),
            "click" | "dblclick" => self.click(verb, a),
            "hover" | "focus" | "scrollintoview" | "scroll-into-view" => self.simple(verb, a),
            "fill" => self.fill(a),
            "type" => self.type_text(a),
            "press" | "key" => self.press(a),
            "scroll" => self.scroll(a),
            "drag" => self.drag(a),
            "get" => self.get(a),
            "wait" => self.wait(a),
            "screenshot" => self.screenshot(a),
            "highlight" => self.highlight(a),
            "exec" => self.exec(a),
            "commands" => self.commands(a),
            "menu" => self.menu(a),
            "logs" => self.logs(a),
            "stats" => self.stats(a),
            "devtools" => self.devtools(a),
            "call" => self.raw_call(a),
            "batch" => self.batch(a),
            "trace" => self.trace(a),
            "demo" => self.demo(a),
            "version" => Ok(Done::new(json!({"version": env!("CARGO_PKG_VERSION")}), format!("ashlar-debug {}", env!("CARGO_PKG_VERSION")))),
            "help" => Ok(Done::new(json!({}), "see `ashlar-debug --help`")),
            other => Err(Fail::Usage(format!("unknown command `{other}`"), "ashlar-debug --help".into())),
        }
    }

    /// Run one verb given as words (batch lines).
    pub(crate) fn run_words(&mut self, words: &[String]) -> Res {
        let Some((verb, rest)) = words.split_first() else { return Ok(Done::new(Value::Null, "")) };
        self.run_verb(verb, Args::new(rest.iter().cloned()), words)
    }

    // ------------------------------------------------------------------ connection

    /// The connection, made on first use.
    pub(crate) fn client(&mut self) -> Result<&mut Client, Fail> {
        if self.conn.is_none() {
            let mut c = self.connect_target()?;
            c.set_timeout(self.timeout);
            self.conn = Some(c);
        }
        self.conn.as_mut().ok_or_else(|| Fail::msg("not connected"))
    }

    fn connect_target(&mut self) -> Result<Client, Fail> {
        if let Some(port) = self.port {
            let dir = match (&self.key_dir, &self.env.key_dir) {
                (Some(d), _) | (None, Some(d)) => d.clone(),
                (None, None) => match session::load(self.env, &self.session).ok().flatten().filter(|s| s.port == port) {
                    Some(s) => s.key_dir,
                    None => {
                        session::find_keys(&self.env.config_roots, Some(port), self.app.as_deref()).into_iter().next().map(|k| k.dir).ok_or_else(
                            || Fail::Failed {
                                error: format!("no control key for port {port} in the usual settings folders"),
                                detail: None,
                                hint: Some("pass --key-dir <the app's settings folder> (where it writes control-key.local-<port>)".into()),
                            },
                        )?
                    }
                },
            };
            return Client::connect(port, &dir).map_err(|e| Fail::control("app.ping", e));
        }
        if let Some(app) = self.app.clone() {
            let keys = session::find_keys(&self.env.config_roots, None, Some(&app));
            for k in &keys {
                if let Ok(c) = session::ping(k.port, &k.dir, Duration::from_secs(3)) {
                    return Ok(c);
                }
            }
            return Err(Fail::Failed {
                error: format!("no running app matching `{app}` answers ({} key file(s) found)", keys.len()),
                detail: None,
                hint: Some("start it with `--control <port>`, or `ashlar-debug open <exe>`".into()),
            });
        }
        let s = self.load_session()?;
        Client::connect(s.port, &s.key_dir).map_err(|e| Fail::Failed {
            error: format!("session `{}` (127.0.0.1:{}) does not answer: {e}", self.session, s.port),
            detail: None,
            hint: Some("the app quit? `ashlar-debug close` forgets the session; `open` starts a new one".into()),
        })
    }

    fn load_session(&self) -> Result<Session, Fail> {
        session::load(self.env, &self.session).map_err(Fail::msg)?.ok_or_else(|| Fail::Failed {
            error: format!("no session `{}`", self.session),
            detail: None,
            hint: Some("start one with `ashlar-debug open <exe>` or `ashlar-debug connect <port>`".into()),
        })
    }

    /// Call one method; the app's error becomes a [`Fail`] with a hint.
    pub(crate) fn call(&mut self, method: &str, params: Value) -> Result<Value, Fail> {
        self.client()?.call(method, params).map_err(|e| Fail::control(method, e))
    }

    /// Call with a longer reply timeout (waits).
    fn call_for(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, Fail> {
        let base = self.timeout;
        let c = self.client()?;
        c.set_timeout(timeout.max(base));
        let r = c.call(method, params).map_err(|e| Fail::control(method, e));
        if let Some(c) = self.conn.as_mut() {
            c.set_timeout(base);
        }
        r
    }

    /// `{}`-returning action with a confirmation line.
    fn act(&mut self, method: &str, params: Value, said: String) -> Res {
        let r = self.call(method, params)?;
        Ok(Done::new(r, said))
    }

    /// Whether screenshots must not raise the window: `ASHLAR_DEBUG_NO_RAISE`, or a session opened
    /// with `--no-raise`.
    fn no_raise(&self) -> bool {
        self.env.no_raise
            || (self.port.is_none() && self.app.is_none() && session::load(self.env, &self.session).ok().flatten().is_some_and(|s| s.no_raise))
    }

    // ------------------------------------------------------------------ sessions

    fn open(&mut self, mut a: Args) -> Res {
        let wait = a.number::<u64>(&["--wait"]).map_err(usage("open"))?.unwrap_or(30_000);
        let no_raise = a.flag(&["--no-raise"]);
        let exe = need(a.positional(), "the app to start", "open")?;
        let rest = a.rest();
        a.finish().map_err(usage("open"))?;
        if let Some(old) = session::load(self.env, &self.session).map_err(Fail::msg)?
            && session::ping(old.port, &old.key_dir, Duration::from_secs(2)).is_ok()
        {
            return Err(Fail::Failed {
                error: format!("session `{}` is open (127.0.0.1:{})", self.session, old.port),
                detail: None,
                hint: Some("`ashlar-debug close` it first, or pick another name with --session".into()),
            });
        }
        let (mut s, c) = session::launch(self.env, &self.session, &exe, &rest, Duration::from_millis(wait)).map_err(Fail::msg)?;
        s.no_raise = no_raise;
        session::save(self.env, &self.session, &s).map_err(Fail::msg)?;
        self.conn = Some(c);
        let text = format!("opened {exe} (pid {}) on 127.0.0.1:{} as session `{}`", s.pid.unwrap_or(0), s.port, self.session);
        Ok(Done::new(json!({"session": self.session, "port": s.port, "pid": s.pid, "key_dir": s.key_dir, "log": s.log}), text))
    }

    fn connect(&mut self, mut a: Args) -> Res {
        let no_raise = a.flag(&["--no-raise"]);
        let port = need(a.positional(), "a port", "connect")?;
        a.finish().map_err(usage("connect"))?;
        let port: u16 = port.parse().map_err(|_| Fail::Usage(format!("`{port}` is not a port"), usage_of("connect")))?;
        self.port = Some(port);
        self.conn = None;
        let mut c = self.connect_target()?;
        let info = c.call("app.version", json!({})).map_err(|e| Fail::control("app.version", e))?;
        // The key folder that worked, for the next invocations.
        let key_dir = match (&self.key_dir, &self.env.key_dir) {
            (Some(d), _) | (None, Some(d)) => d.clone(),
            (None, None) => session::load(self.env, &self.session)
                .ok()
                .flatten()
                .filter(|s| s.port == port)
                .map(|s| s.key_dir)
                .or_else(|| session::find_keys(&self.env.config_roots, Some(port), self.app.as_deref()).into_iter().next().map(|k| k.dir))
                .unwrap_or_default(),
        };
        let s = Session { port, key_dir, pid: None, exe: None, log: None, launched: false, trace: None, no_raise };
        session::save(self.env, &self.session, &s).map_err(Fail::msg)?;
        self.conn = Some(c);
        let name = info.get("name").and_then(Value::as_str).unwrap_or("app");
        let version = info.get("version").and_then(Value::as_str).unwrap_or("");
        Ok(Done::new(
            json!({"session": self.session, "port": port, "app": info}),
            format!("connected to {name} {version} on 127.0.0.1:{port} as session `{}`", self.session),
        ))
    }

    fn close(&mut self, mut a: Args) -> Res {
        let force_quit = a.flag(&["--quit"]);
        a.finish().map_err(usage("close"))?;
        let s = self.load_session()?;
        let mut quit = false;
        if s.launched || force_quit {
            // The app may close the connection as it quits: that is success too.
            if let Ok(mut c) = Client::connect(s.port, &s.key_dir) {
                c.set_timeout(Duration::from_secs(5));
                let _ = c.call("app.quit", json!({"discard": true}));
                quit = true;
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut gone = false;
            while std::time::Instant::now() < deadline {
                if session::ping(s.port, &s.key_dir, Duration::from_millis(500)).is_err() {
                    gone = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            if !gone && let Some(pid) = s.pid.filter(|_| s.launched) {
                session::kill(pid);
            }
        }
        self.conn = None;
        session::remove(self.env, &self.session).map_err(Fail::msg)?;
        let text = if quit { format!("closed session `{}` (the app quit)", self.session) } else { format!("forgot session `{}`", self.session) };
        Ok(Done::new(json!({"session": self.session, "quit": quit}), text))
    }

    fn status(&mut self, a: Args) -> Res {
        a.finish().map_err(usage("status"))?;
        let s = session::load(self.env, &self.session).map_err(Fail::msg)?;
        let info = self.call("app.version", json!({}))?;
        let name = info.get("name").and_then(Value::as_str).unwrap_or("app");
        let version = info.get("version").and_then(Value::as_str).unwrap_or("");
        let methods: Vec<&str> =
            info.get("methods").and_then(Value::as_array).map(|m| m.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        let port = self.conn.as_ref().map(Client::port).unwrap_or(0);
        let mut text = format!("{name} {version} on 127.0.0.1:{port}");
        if let Some(s) = &s
            && self.port.is_none()
            && self.app.is_none()
        {
            text.push_str(&format!("\nsession `{}`", self.session));
            if let Some(pid) = s.pid {
                text.push_str(&format!(", pid {pid}"));
            }
            if let Some(t) = &s.trace {
                text.push_str(&format!(", tracing to {} ({} steps)", t.dir.display(), t.step));
            }
        }
        text.push_str(&format!("\nmethods: {}", methods.join(" ")));
        Ok(Done::new(json!({"session": self.session, "app": info, "record": s}), text))
    }

    fn sessions(&mut self, a: Args) -> Res {
        a.finish().map_err(usage("sessions"))?;
        let mut out = Vec::new();
        let mut text = String::new();
        for name in session::list(self.env) {
            let Ok(Some(s)) = session::load(self.env, &name) else { continue };
            let alive = session::ping(s.port, &s.key_dir, Duration::from_secs(2)).is_ok();
            text.push_str(&format!("{name:<16} 127.0.0.1:{:<6} {}\n", s.port, if alive { "running" } else { "not answering" }));
            out.push(json!({"name": name, "port": s.port, "pid": s.pid, "alive": alive}));
        }
        if text.is_empty() {
            text = "no sessions".into();
        }
        Ok(Done::new(json!(out), text))
    }

    // ------------------------------------------------------------------ reading the UI

    fn snapshot(&mut self, mut a: Args) -> Res {
        let interactive = a.flag(&["-i", "--interactive"]);
        let compact = a.flag(&["-c", "--compact"]);
        let scope = a.value(&["--scope", "-s"]).map_err(usage("snapshot"))?;
        let depth = a.number::<u32>(&["--depth", "-d"]).map_err(usage("snapshot"))?;
        a.finish().map_err(usage("snapshot"))?;
        let mut p = json!({"interactive": interactive});
        if compact {
            p["compact"] = json!(true);
        }
        if let Some(s) = scope {
            p["scope"] = parse_target(&s, "snapshot")?;
        }
        if let Some(d) = depth {
            p["depth"] = json!(d);
        }
        let v = self.call("ui.snapshot", p)?;
        let text = outline(&v);
        Ok(Done::new(v, text))
    }

    fn get(&mut self, mut a: Args) -> Res {
        let what =
            need(a.positional(), "what to read (text, value, name, role, rect, enabled, visible, focused, checked, selected, element)", "get")?;
        let t = need(a.positional(), "a target", "get")?;
        a.finish().map_err(usage("get"))?;
        if !GET_WHAT.contains(&what.as_str()) {
            return Err(Fail::Usage(format!("cannot get `{what}`"), usage_of("get")));
        }
        let v = self.call("ui.get", json!({"target": parse_target(&t, "get")?, "what": what}))?;
        let value = v.get("value").cloned().unwrap_or(Value::Null);
        let text = plain(&value);
        Ok(Done::new(value, text))
    }

    // ------------------------------------------------------------------ acting

    fn click(&mut self, verb: &str, mut a: Args) -> Res {
        let button = a.value(&["--button"]).map_err(usage(verb))?;
        let count = a.number::<u32>(&["--count"]).map_err(usage(verb))?;
        let mods = a.value(&["--mods", "--modifiers"]).map_err(usage(verb))?;
        let t = need(a.positional(), "a target", verb)?;
        a.finish().map_err(usage(verb))?;
        let t = parse_target(&t, verb)?;
        let mut p = json!({"target": t});
        if let Some(b) = button {
            let b = match b.as_str() {
                "left" | "primary" => "primary",
                "right" | "secondary" => "secondary",
                "middle" => "middle",
                other => return Err(Fail::Usage(format!("unknown button `{other}`"), usage_of(verb))),
            };
            p["button"] = json!(b);
        }
        let count = if verb == "dblclick" { Some(2) } else { count };
        if let Some(n) = count {
            if !(1..=3).contains(&n) {
                return Err(Fail::Usage("--count is 1, 2 or 3".into(), usage_of(verb)));
            }
            p["count"] = json!(n);
        }
        if let Some(m) = mods {
            p["modifiers"] = modifiers(&m).map_err(usage(verb))?;
        }
        let said = format!("{}ed {}", if verb == "dblclick" { "double-click" } else { "click" }, target::show(&t));
        self.act("ui.click", p, said)
    }

    /// `hover`, `focus`, `scrollintoview`: a target and nothing else.
    fn simple(&mut self, verb: &str, mut a: Args) -> Res {
        let verb = if verb == "scroll-into-view" { "scrollintoview" } else { verb };
        let t = need(a.positional(), "a target", verb)?;
        a.finish().map_err(usage(verb))?;
        let t = parse_target(&t, verb)?;
        let (method, past) = match verb {
            "hover" => ("ui.hover", "hovered"),
            "focus" => ("ui.focus", "focused"),
            _ => ("ui.scroll_into_view", "scrolled to"),
        };
        let said = format!("{past} {}", target::show(&t));
        self.act(method, json!({"target": t}), said)
    }

    fn fill(&mut self, mut a: Args) -> Res {
        let submit = a.flag(&["--submit"]);
        let t = need(a.positional(), "a target", "fill")?;
        let text = need(a.positional(), "the text", "fill")?;
        a.finish().map_err(usage("fill"))?;
        let t = parse_target(&t, "fill")?;
        let said = format!("filled {}", target::show(&t));
        self.act("ui.fill", json!({"target": t, "text": text, "submit": submit}), said)
    }

    fn type_text(&mut self, mut a: Args) -> Res {
        let text = need(a.positional(), "the text", "type")?;
        a.finish().map_err(usage("type"))?;
        let said = format!("typed {} characters", text.chars().count());
        self.act("ui.type", json!({"text": text}), said)
    }

    fn press(&mut self, mut a: Args) -> Res {
        let keys = a.rest().join(" ");
        a.finish().map_err(usage("press"))?;
        if keys.trim().is_empty() {
            return Err(Fail::Usage("press needs keys, like Mod+S".into(), usage_of("press")));
        }
        let said = format!("pressed {keys}");
        self.act("ui.press", json!({"keys": keys}), said)
    }

    fn scroll(&mut self, mut a: Args) -> Res {
        let dx = a.number::<f64>(&["--dx"]).map_err(usage("scroll"))?;
        let dy = a.number::<f64>(&["--dy"]).map_err(usage("scroll"))?;
        let mut words = a.rest();
        a.finish().map_err(usage("scroll"))?;
        let dirs = ["up", "down", "left", "right"];
        let t = match words.first() {
            Some(w) if !dirs.contains(&w.as_str()) && w.parse::<f64>().is_err() => Some(parse_target(&words.remove(0), "scroll")?),
            _ => None,
        };
        let (mut x, mut y) = (dx.unwrap_or(0.0), dy.unwrap_or(0.0));
        match words.as_slice() {
            [] => {
                if dx.is_none() && dy.is_none() {
                    y = 100.0;
                }
            }
            [d] | [d, _] if dirs.contains(&d.as_str()) => {
                let px = match words.get(1) {
                    Some(n) => n
                        .parse::<f64>()
                        .ok()
                        .filter(|n| n.is_finite())
                        .ok_or_else(|| Fail::Usage(format!("`{n}` is not a distance"), usage_of("scroll")))?,
                    None => 100.0,
                };
                match d.as_str() {
                    "up" => y = -px,
                    "down" => y = px,
                    "left" => x = -px,
                    _ => x = px,
                }
            }
            _ => return Err(Fail::Usage(format!("unexpected `{}`", words.join(" ")), usage_of("scroll"))),
        }
        let mut p = json!({"dx": x, "dy": y});
        if let Some(t) = &t {
            p["target"] = t.clone();
        }
        let said = format!("scrolled by {x},{y}{}", t.as_ref().map(|t| format!(" over {}", target::show(t))).unwrap_or_default());
        self.act("ui.scroll", p, said)
    }

    fn drag(&mut self, mut a: Args) -> Res {
        let steps = a.number::<u32>(&["--steps"]).map_err(usage("drag"))?;
        let from = need(a.positional(), "where to start", "drag")?;
        let to = need(a.positional(), "where to drop", "drag")?;
        a.finish().map_err(usage("drag"))?;
        let (from, to) = (parse_target(&from, "drag")?, parse_target(&to, "drag")?);
        let mut p = json!({"from": from, "to": to});
        if let Some(s) = steps {
            p["steps"] = json!(s.clamp(1, 1000));
        }
        let said = format!("dragged {} to {}", target::show(&from), target::show(&to));
        self.act("ui.drag", p, said)
    }

    fn highlight(&mut self, mut a: Args) -> Res {
        let ms = a.number::<u64>(&["--ms"]).map_err(usage("highlight"))?;
        let t = need(a.positional(), "a target", "highlight")?;
        a.finish().map_err(usage("highlight"))?;
        let t = parse_target(&t, "highlight")?;
        let mut p = json!({"target": t});
        if let Some(ms) = ms {
            p["ms"] = json!(ms);
        }
        let said = format!("highlighted {}", target::show(&t));
        self.act("ui.highlight", p, said)
    }

    fn wait(&mut self, mut a: Args) -> Res {
        let timeout = a.number::<u64>(&["--timeout", "-t"]).map_err(usage("wait"))?.unwrap_or(5000);
        let text = a.value(&["--text"]).map_err(usage("wait"))?;
        let gone = a.value(&["--gone"]).map_err(usage("wait"))?;
        let enabled = a.value(&["--enabled"]).map_err(usage("wait"))?;
        let element = a.value(&["--ref", "--element"]).map_err(usage("wait"))?;
        let idle = a.flag(&["--idle"]);
        let pos = a.positional();
        a.finish().map_err(usage("wait"))?;
        // `wait 500`: just sleep.
        if let Some(ms) = pos.as_deref().and_then(|p| p.parse::<u64>().ok()) {
            if text.is_some() || gone.is_some() || enabled.is_some() || element.is_some() || idle {
                return Err(Fail::Usage("wait takes a duration or a condition, not both".into(), usage_of("wait")));
            }
            std::thread::sleep(Duration::from_millis(ms.min(600_000)));
            return Ok(Done::new(json!({"elapsed_ms": ms}), format!("waited {ms} ms")));
        }
        let element = element.or(pos);
        let mut p = match (text, gone, enabled, element, idle) {
            (Some(t), None, None, None, false) => json!({"for": "text", "text": t}),
            (None, Some(g), None, None, false) => json!({"for": "gone", "target": parse_target(&g, "wait")?}),
            (None, None, Some(e), None, false) => json!({"for": "enabled", "target": parse_target(&e, "wait")?}),
            (None, None, None, Some(e), false) => json!({"for": "element", "target": parse_target(&e, "wait")?}),
            (None, None, None, None, true) => json!({"for": "idle"}),
            (None, None, None, None, false) => return Err(Fail::Usage("wait for what?".into(), usage_of("wait"))),
            _ => return Err(Fail::Usage("wait for one condition at a time".into(), usage_of("wait"))),
        };
        p["timeout_ms"] = json!(timeout);
        let v = self.call_for("ui.wait", p, Duration::from_millis(timeout.saturating_add(10_000)))?;
        let ms = v.get("elapsed_ms").and_then(Value::as_u64).unwrap_or(0);
        Ok(Done::new(v, format!("waited {ms} ms")))
    }

    fn screenshot(&mut self, mut a: Args) -> Res {
        let annotate = a.flag(&["--annotate", "-a"]);
        let no_raise = a.flag(&["--no-raise"]);
        let tgt = a.value(&["--target", "--ref"]).map_err(usage("screenshot"))?;
        let highlight = a.value(&["--highlight"]).map_err(usage("screenshot"))?;
        let pad = a.number::<f64>(&["--pad"]).map_err(usage("screenshot"))?;
        let path = a.positional();
        a.finish().map_err(usage("screenshot"))?;
        let mut p = json!({});
        if annotate {
            p["annotate"] = json!(true);
        }
        if no_raise || self.no_raise() {
            p["raise"] = json!(false);
        }
        if let Some(t) = tgt {
            p["target"] = parse_target(&t, "screenshot")?;
        }
        if let Some(h) = highlight {
            p["highlight"] = parse_target(&h, "screenshot")?;
        }
        if let Some(pad) = pad {
            p["pad"] = json!(pad);
        }
        let v = self.call("ui.screenshot", p)?;
        let path = path.map(PathBuf::from).unwrap_or_else(|| self.env.home.join("screenshots").join(format!("shot-{}.png", unix_ms())));
        save_png(&v, &path).map_err(Fail::msg)?;
        let mut text = path.display().to_string();
        let annotations = v.get("annotations").cloned().unwrap_or(Value::Null);
        for an in annotations.as_array().into_iter().flatten() {
            let r = an.get("ref").and_then(Value::as_str).unwrap_or("?");
            let rect = an.get("rect").map(|r| r.to_string()).unwrap_or_default();
            text.push_str(&format!("\n@{r} {rect}"));
        }
        let mut result = json!({"path": path, "width": v.get("width"), "height": v.get("height")});
        if !annotations.is_null() {
            result["annotations"] = annotations;
        }
        Ok(Done::new(result, text))
    }

    // ------------------------------------------------------------------ the app

    fn exec(&mut self, mut a: Args) -> Res {
        let id = need(a.positional(), "a command id", "exec")?;
        let raw = a.positional();
        a.finish().map_err(usage("exec"))?;
        let params = json_arg(raw, self.io.input).map_err(usage("exec"))?;
        let v = self.call("engine.execute", json!({"command": id, "params": params}))?;
        let text = plain(&v);
        Ok(Done::new(v, text))
    }

    fn commands(&mut self, mut a: Args) -> Res {
        let q = a.rest().join(" ");
        a.finish().map_err(usage("commands"))?;
        let v = self.call("engine.commands", json!({"query": q}))?;
        let list: Vec<CommandInfo> = serde_json::from_value(v.clone()).unwrap_or_default();
        let width = list.iter().map(|c| c.id.chars().count()).max().unwrap_or(0);
        let mut text = String::new();
        for c in &list {
            let mut line = format!("{:<width$}  {}", c.id, c.label);
            if let Some(s) = &c.shortcut {
                line.push_str(&format!("  [{s}]"));
            }
            if !c.enabled {
                line.push_str(&format!("  (disabled: {})", c.disabled_reason.as_deref().unwrap_or("not available")));
            }
            text.push_str(line.trim_end());
            text.push('\n');
        }
        Ok(Done::new(v, text))
    }

    fn menu(&mut self, mut a: Args) -> Res {
        let id = a.positional();
        a.finish().map_err(usage("menu"))?;
        if let Some(id) = id {
            let v = self.call("ui.menu.invoke", json!({"id": id}))?;
            return Ok(Done::new(v, format!("invoked {id}")));
        }
        let v = self.call("ui.menu.list", json!({}))?;
        let mut text = String::new();
        menu_lines(&v, 0, &mut text);
        Ok(Done::new(v, text))
    }

    fn logs(&mut self, mut a: Args) -> Res {
        let lines = a.number::<u64>(&["--lines", "-n"]).map_err(usage("logs"))?.unwrap_or(50);
        let level = a.value(&["--level"]).map_err(usage("logs"))?;
        a.finish().map_err(usage("logs"))?;
        let mut p = json!({"limit": lines});
        if let Some(l) = &level {
            p["level"] = json!(l);
        }
        match self.call("app.logs", p) {
            Ok(v) => {
                let mut text = String::new();
                for l in v.get("lines").and_then(Value::as_array).into_iter().flatten() {
                    let s = |k: &str| l.get(k).and_then(Value::as_str).unwrap_or("");
                    text.push_str(&format!("{:<5} {}: {}\n", s("level"), s("target"), s("message")));
                }
                Ok(Done::new(v, text))
            }
            // An app without `app.logs`: what `open` captured of its output.
            Err(Fail::Failed { error, .. }) if error.starts_with("unknown method") => {
                let log = session::load(self.env, &self.session).ok().flatten().and_then(|s| s.log);
                match log {
                    Some(p) => {
                        let t = session::tail(&p, lines.min(10_000) as usize);
                        Ok(Done::new(json!({"file": p, "text": t}), t))
                    }
                    None => Err(Fail::msg("this app has no `app.logs`, and its output was not captured (it was not started with `open`)")),
                }
            }
            Err(e) => Err(e),
        }
    }

    fn stats(&mut self, mut a: Args) -> Res {
        let reset = a.flag(&["--reset"]);
        a.finish().map_err(usage("stats"))?;
        let v = self.call("app.stats", if reset { json!({"reset": true}) } else { json!({}) })?;
        let text = serde_json::to_string_pretty(&v).unwrap_or_default();
        Ok(Done::new(v, text))
    }

    fn devtools(&mut self, mut a: Args) -> Res {
        let what = a.positional().unwrap_or_else(|| "toggle".into());
        a.finish().map_err(usage("devtools"))?;
        let v = match what.as_str() {
            "open" => self.call("devtools.toggle", json!({"open": true}))?,
            "close" => self.call("devtools.toggle", json!({"open": false}))?,
            "toggle" => self.call("devtools.toggle", json!({}))?,
            "state" => self.call("devtools.state", json!({}))?,
            other => return Err(Fail::Usage(format!("devtools {other}?"), usage_of("devtools"))),
        };
        let text = match v.get("open").and_then(Value::as_bool) {
            Some(true) => "devtools open".to_string(),
            Some(false) => "devtools closed".to_string(),
            None => v.to_string(),
        };
        Ok(Done::new(v, text))
    }

    fn raw_call(&mut self, mut a: Args) -> Res {
        let method = need(a.positional(), "a method", "call")?;
        let raw = a.positional();
        a.finish().map_err(usage("call"))?;
        let params = json_arg(raw, self.io.input).map_err(usage("call"))?;
        let v = self.call(&method, params)?;
        let text = serde_json::to_string_pretty(&v).unwrap_or_default();
        Ok(Done::new(v, text))
    }

    fn demo(&mut self, mut a: Args) -> Res {
        let port = a.number::<u16>(&["--control"]).map_err(usage("demo"))?;
        // An app ignores arguments it does not know; so does the demo (it is launched like one).
        let _ = a.rest();
        let key_dir = self.key_dir.clone().or_else(|| self.env.key_dir.clone());
        crate::demo::run(port, key_dir.as_deref(), self.io.out).map_err(Fail::msg)?;
        Ok(Done::new(json!({}), ""))
    }

    // ------------------------------------------------------------------ trace

    fn trace(&mut self, mut a: Args) -> Res {
        let what = need(a.positional(), "start <dir> or stop", "trace")?;
        let dir = a.positional();
        a.finish().map_err(usage("trace"))?;
        let mut s = self.load_session()?;
        match (what.as_str(), dir) {
            ("start", Some(d)) => {
                let d = PathBuf::from(d);
                std::fs::create_dir_all(&d).map_err(|e| Fail::msg(format!("cannot create {}: {e}", d.display())))?;
                let d = d.canonicalize().unwrap_or(d);
                s.trace = Some(Trace { dir: d.clone(), step: 0 });
                session::save(self.env, &self.session, &s).map_err(Fail::msg)?;
                Ok(Done::new(json!({"dir": d}), format!("tracing to {}", d.display())))
            }
            ("stop", None) => {
                let t = s.trace.take().ok_or_else(|| Fail::msg("no trace is running"))?;
                session::save(self.env, &self.session, &s).map_err(Fail::msg)?;
                Ok(Done::new(json!({"dir": t.dir, "steps": t.step}), format!("{} steps in {}", t.step, t.dir.display())))
            }
            _ => Err(Fail::Usage("trace start <dir> | trace stop".into(), usage_of("trace"))),
        }
    }

    /// After an action in a traced session: `NNN-verb.png`, `NNN-verb.txt` (the snapshot) and a
    /// line in `steps.jsonl`. Problems are reported on stderr; the action's outcome stands.
    pub(crate) fn record(&mut self, verb: &str, words: &[String], outcome: &Res) {
        if self.port.is_some() || self.app.is_some() {
            return;
        }
        let Ok(Some(mut s)) = session::load(self.env, &self.session) else { return };
        let Some(t) = s.trace.as_mut() else { return };
        t.step = t.step.saturating_add(1);
        let (n, dir) = (t.step, t.dir.clone());
        let base = format!("{n:03}-{verb}");
        let mut problems = Vec::new();
        let png = dir.join(format!("{base}.png"));
        let shot = if self.no_raise() { json!({"raise": false}) } else { json!({}) };
        match self.call("ui.screenshot", shot).map_err(|e| e.text()).and_then(|v| save_png(&v, &png)) {
            Ok(()) => {}
            Err(e) => problems.push(format!("screenshot: {e}")),
        }
        let txt = dir.join(format!("{base}.txt"));
        match self.call("ui.snapshot", json!({})).map_err(|e| e.text()) {
            Ok(v) => {
                if let Err(e) = std::fs::write(&txt, outline(&v) + "\n") {
                    problems.push(format!("snapshot: {e}"));
                }
            }
            Err(e) => problems.push(format!("snapshot: {e}")),
        }
        let mut line = json!({
            "step": n,
            "command": words.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" "),
            "ok": outcome.is_ok(),
            "png": png.file_name().map(|f| f.to_string_lossy().to_string()),
            "snapshot": txt.file_name().map(|f| f.to_string_lossy().to_string()),
            "unix_ms": unix_ms() as u64,
        });
        if let Err(e) = outcome {
            line["error"] = json!(e.text());
        }
        let appended = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("steps.jsonl")).and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = appended {
            problems.push(format!("steps.jsonl: {e}"));
        }
        if let Err(e) = session::save(self.env, &self.session, &s) {
            problems.push(e);
        }
        for p in problems {
            let _ = writeln!(self.io.err, "trace: {p}");
        }
    }
}

/// Decode a `ui.screenshot` reply's PNG and write it.
fn save_png(v: &Value, path: &Path) -> Result<(), String> {
    let b64 = v.get("png").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or("the reply carried no picture")?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim()).map_err(|e| format!("the picture is not base64: {e}"))?;
    if let Some(d) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// A menu tree as indented lines (any `[{label|title, items|children, shortcut?, enabled?, id?}]`).
fn menu_lines(v: &Value, depth: usize, out: &mut String) {
    let items = match v {
        Value::Array(a) => a.clone(),
        Value::Object(o) => o.get("menus").or(o.get("items")).and_then(Value::as_array).cloned().unwrap_or_default(),
        _ => Vec::new(),
    };
    for it in items.iter().take(10_000) {
        let s = |k: &str| it.get(k).and_then(Value::as_str);
        let label = s("label").or(s("title")).or(s("name")).unwrap_or("?");
        let mut line = format!("{}{label}", "  ".repeat(depth));
        if let Some(id) = s("id") {
            line.push_str(&format!("  ({id})"));
        }
        if let Some(sc) = s("shortcut") {
            line.push_str(&format!("  [{sc}]"));
        }
        if it.get("enabled") == Some(&json!(false)) {
            line.push_str("  (disabled)");
        }
        out.push_str(&line);
        out.push('\n');
        if let Some(children) = it.get("items").or(it.get("children")).filter(|c| c.is_array())
            && depth < 16
        {
            menu_lines(children, depth.saturating_add(1), out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_names() {
        assert_eq!(modifiers("Shift+Mod").unwrap(), json!({"shift": true, "command": true}));
        assert_eq!(modifiers("alt+ctrl").unwrap(), json!({"alt": true, "ctrl": true}));
        assert!(modifiers("Hyper").is_err());
    }

    #[test]
    fn outline_from_elements_when_there_is_no_tree() {
        let v = json!({"elements": [
            {"ref": "e1", "role": "window", "name": "W"},
            {"ref": "e2", "role": "button", "name": "Save", "parent": "e1", "enabled": false}
        ]});
        assert_eq!(outline(&v), "- window \"W\" [ref=e1]\n  - button \"Save\" [ref=e2] [disabled]");
        assert_eq!(outline(&json!({"tree": "- x\n"})), "- x");
    }

    #[test]
    fn menus_print_as_a_tree() {
        let mut s = String::new();
        menu_lines(&json!([{"label": "File", "items": [{"label": "Save", "id": "file.save", "shortcut": "Mod+S", "enabled": false}]}]), 0, &mut s);
        assert_eq!(s, "File\n  Save  (file.save)  [Mod+S]  (disabled)\n");
    }
}
