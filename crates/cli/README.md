# ashlar-cli

The standard verbs of an app's command-line tool (`<tool>`, the name given to `Cli::new`), so every app is scriptable the same way:

```text
<tool> commands [--json] [--query Q]     list commands
<tool> describe <id>                     one command in full (parameter schema)
<tool> exec <id> [json|-]                run one command
<tool> run <file.jsonl|-> [--keep-going] run {"command","params"} lines
<tool> mcp [--isolate] [--per-command]   MCP server on stdio
<tool> serve                             control protocol on stdio (worker for `mcp --isolate`)
<tool> version [--json]
```

`--connect PORT` (on `commands`, `describe`, `exec`, `run`, `mcp`) drives the running app through
its control port instead of a fresh headless session. Replies are one line,
`{"ok":true,"result":…}` / `{"ok":false,"error":"…"}`; exit codes 0 ok, 1 failed, 2 usage.

```rust
fn main() -> std::process::ExitCode {
    ashlar_cli::Cli::new("notes", env!("CARGO_PKG_VERSION"), || Box::new(Session::new()))
        .key_dir(settings_dir())
        .instructions("A note-taking app. …")
        .verb("export", "export <file>", "export all notes", |args, io| { /* … */ ashlar_cli::Exit::Ok })
        .run()
}
```

Argument parsing is a tiny built-in parser (`Args`: flags, `--opt v`/`--opt=v`, positionals, `--`),
also handed to app verbs. Tests: `tests/cli.rs`.
