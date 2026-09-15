use std::path::Path;

use super::*;

fn setup(cmd: &str) -> Setup {
    Setup {
        id: "app".into(),
        title: "App".into(),
        server: Server { command: cmd.into(), args: vec!["mcp".into()] },
        skills: vec![Skill::new("app", "Use App for editing.", "Start here."), Skill::new("app-review", "Check a cut.", "Review it.")],
    }
}

fn write(p: &Path, t: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, t).unwrap();
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

/// Every file under `dir` with its text, for comparing whole trees.
fn snapshot(dir: &Path) -> Vec<(String, String)> {
    fn walk(root: &Path, d: &Path, out: &mut Vec<(String, String)>) {
        let mut es: Vec<_> = std::fs::read_dir(d).into_iter().flatten().flatten().collect();
        es.sort_by_key(|e| e.file_name());
        for e in es {
            let p = e.path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                out.push((p.strip_prefix(root).unwrap().display().to_string(), read(&p)));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

fn without_backups(v: Vec<(String, String)>) -> Vec<(String, String)> {
    v.into_iter().filter(|(p, _)| !p.contains("-backup-")).collect()
}

const OTHER_JSON: &str =
    "{\n  // mine\n  \"mcpServers\": {\n    \"other\": {\"command\": \"/opt/other\", \"args\": []}\n  },\n  \"numStartups\": 12\n}\n";
const OTHER_TOML: &str = "# my codex config\nmodel = \"o4\"\n\n[mcp_servers.other]\ncommand = \"/opt/other\" # keep\nargs = []\n";

/// Existing configs with someone else's server, for every agent.
fn fixtures(home: &Path) {
    write(&home.join(".claude.json"), OTHER_JSON);
    write(&home.join(".codex/config.toml"), OTHER_TOML);
    write(&home.join(".cursor/mcp.json"), OTHER_JSON);
    write(&home.join(".codeium/windsurf/mcp_config.json"), OTHER_JSON);
    write(&home.join(".config/devin/mcp_config.json"), OTHER_JSON);
    write(
        &home.join("Library/Application Support/Code/User/mcp.json"),
        "{\n  \"servers\": {\n    \"other\": {\"type\": \"stdio\", \"command\": \"/opt/other\"}\n  },\n  \"inputs\": []\n}\n",
    );
    write(&home.join(".gemini/settings.json"), "{\"theme\": \"x\", \"mcpServers\": {\"other\": {\"command\": \"/opt/other\"}}}");
    // a skill of the user's own that shares our prefix
    write(&home.join(".claude/skills/app-mine/SKILL.md"), "---\nname: app-mine\ndescription: mine\n---\nmine\n");
}

#[test]
fn installs_into_every_agent_keeping_other_entries_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    fixtures(home);
    let before = snapshot(home);
    let host = Host::at(home, Os::Mac);
    let s = setup("/usr/local/bin/app");
    for agent in Agent::ALL {
        let plan = install(agent, &Scope::User, &s, &host).unwrap();
        assert!(!plan.is_noop(), "{agent}");
        plan.apply("app").unwrap();
        assert!(install(agent, &Scope::User, &s, &host).unwrap().is_noop(), "{agent}: a second install changes nothing");
        assert_eq!(state(agent, &Scope::User, &s, &host).unwrap(), State::Current);
    }
    // the servers, merged next to the user's
    let claude = read(&home.join(".claude.json"));
    assert_eq!(
        jsonc::get(&claude, &["mcpServers", "app"]).unwrap(),
        Some(json!({"type": "stdio", "command": "/usr/local/bin/app", "args": ["mcp"]}))
    );
    assert!(claude.contains("// mine") && claude.contains("\"numStartups\": 12") && claude.contains("\"other\""));
    assert_eq!(jsonc::get(&read(&home.join(".cursor/mcp.json")), &["mcpServers", "app", "command"]).unwrap(), Some(json!("/usr/local/bin/app")));
    let vscode = read(&home.join("Library/Application Support/Code/User/mcp.json"));
    assert_eq!(jsonc::get(&vscode, &["servers", "app", "type"]).unwrap(), Some(json!("stdio")));
    assert!(vscode.contains("\"inputs\": []"));
    let codex = read(&home.join(".codex/config.toml"));
    assert!(codex.starts_with(OTHER_TOML), "{codex}");
    assert!(codex.contains("[mcp_servers.app]\ncommand = \"/usr/local/bin/app\"\nargs = [\"mcp\"]\n"), "{codex}");
    let gemini: Value = serde_json::from_str(&read(&home.join(".gemini/settings.json"))).unwrap();
    assert_eq!(gemini["mcpServers"]["app"], json!({"command": "/usr/local/bin/app", "args": ["mcp"]}));
    assert_eq!(gemini["theme"], "x");
    // skills, marked; the user's own skill is untouched
    let skill = read(&home.join(".claude/skills/app-review/SKILL.md"));
    assert!(skill.starts_with("---\nname: app-review\ndescription: Check a cut.\n---\n<!-- Installed by `app agent install`"), "{skill}");
    assert!(home.join(".agents/skills/app/SKILL.md").is_file());
    // Windsurf's legacy file too, since its folder is there
    assert!(read(&home.join(".codeium/windsurf/mcp_config.json")).contains("/usr/local/bin/app"));
    assert!(read(&home.join(".config/devin/mcp_config.json")).contains("/usr/local/bin/app"));
    assert_eq!(read(&home.join(".claude/skills/app-mine/SKILL.md")), "---\nname: app-mine\ndescription: mine\n---\nmine\n");
    // each user file that changed was backed up once, with its old text
    let backups: Vec<_> = snapshot(home).into_iter().filter(|(p, _)| p.contains(".app-backup-")).collect();
    assert!(backups.iter().any(|(p, t)| p.starts_with(".claude.json.app-backup-") && t == OTHER_JSON), "{backups:?}");
    assert_eq!(backups.iter().filter(|(p, _)| p.starts_with(".claude.json")).count(), 1);
    // uninstall restores every file exactly
    for agent in Agent::ALL {
        uninstall(agent, &Scope::User, "app", &host).unwrap().apply("app").unwrap();
        assert_eq!(state(agent, &Scope::User, &s, &host).unwrap(), State::Missing, "{agent}");
    }
    assert_eq!(without_backups(snapshot(home)), before);
}

#[test]
fn a_new_version_updates_moves_and_drops_skills() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::at(dir.path(), Os::Mac);
    let mut s = setup("/a/app");
    install(Agent::Claude, &Scope::User, &s, &host).unwrap().apply("app").unwrap();
    s.server.command = "/b/app".into();
    s.skills.truncate(1);
    s.skills.push(Skill::new("app-new", "New.", "New body."));
    assert_eq!(state(Agent::Claude, &Scope::User, &s, &host).unwrap(), State::Outdated);
    let plan = install(Agent::Claude, &Scope::User, &s, &host).unwrap();
    let acts: Vec<(String, Action)> = plan.changes.iter().map(|c| (c.what.clone(), c.action)).collect();
    assert_eq!(
        acts,
        [
            ("MCP server `app`".to_string(), Action::Update),
            ("skill `app`".to_string(), Action::Unchanged),
            ("skill `app-new`".to_string(), Action::Create),
            ("skill `app-review`".to_string(), Action::Remove),
        ]
    );
    plan.apply("app").unwrap();
    assert!(!dir.path().join(".claude/skills/app-review").exists(), "the empty skill folder goes too");
    assert_eq!(jsonc::get(&read(&dir.path().join(".claude.json")), &["mcpServers", "app", "command"]).unwrap(), Some(json!("/b/app")));
}

#[test]
fn project_scope_writes_into_the_project() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let proj = dir.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let host = Host::at(&home, Os::Linux);
    let s = setup("/x/app");
    let scope = Scope::Project(proj.clone());
    let mut written = Vec::new();
    for agent in Agent::ALL {
        for a in install(agent, &scope, &s, &host).unwrap().apply("app").unwrap() {
            written.push(a.path.strip_prefix(dir.path()).unwrap().display().to_string());
        }
    }
    for want in [
        "proj/.mcp.json",
        "proj/.claude/skills/app/SKILL.md",
        "proj/.codex/config.toml",
        "proj/.agents/skills/app/SKILL.md",
        "proj/.agents/skills/app-review/SKILL.md",
        "proj/.cursor/mcp.json",
        "proj/.devin/mcp_config.json",
        "proj/.vscode/mcp.json",
        "proj/.gemini/settings.json",
    ] {
        assert!(written.iter().any(|w| w == want), "{want} not in {written:?}");
    }
    assert!(!home.exists(), "a project install writes nothing in the home folder");
    // the shared skills stay while another agent still uses them
    let p = uninstall(Agent::Codex, &scope, "app", &host).unwrap();
    assert!(p.notes.iter().any(|n| n.contains("kept the skills")), "{:?}", p.notes);
    p.apply("app").unwrap();
    assert!(proj.join(".agents/skills/app/SKILL.md").is_file());
    for agent in Agent::ALL {
        uninstall(agent, &scope, "app", &host).unwrap().apply("app").unwrap();
    }
    assert!(!proj.join(".agents/skills/app").exists() && !proj.join(".claude/skills/app").exists());
}

#[test]
fn leaves_what_is_not_ours() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let host = Host::at(home, Os::Mac);
    // an entry called `app` that runs something else, and a broken config
    write(&home.join(".cursor/mcp.json"), "{\"mcpServers\": {\"app\": {\"command\": \"/opt/another-tool\"}}}");
    let plan = uninstall(Agent::Cursor, &Scope::User, "app", &host).unwrap();
    assert!(plan.is_noop());
    assert!(plan.notes.iter().any(|n| n.contains("another-tool")), "{:?}", plan.notes);
    write(&home.join(".gemini/settings.json"), "{ not json");
    let e = install(Agent::Gemini, &Scope::User, &setup("/x/app"), &host).unwrap_err();
    assert!(e.to_string().contains("nothing was changed"), "{e}");
    assert_eq!(read(&home.join(".gemini/settings.json")), "{ not json");
    write(&home.join(".codex/config.toml"), "mcp_servers = 3\n");
    assert!(install(Agent::Codex, &Scope::User, &setup("/x/app"), &host).is_err());
}

#[test]
fn print_lists_files_and_our_part_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::at(dir.path(), Os::Mac);
    let plan = install(Agent::Codex, &Scope::User, &setup("/x/app"), &host).unwrap();
    let text = plan.describe(true);
    assert!(text.contains("create    ") && text.contains("config.toml") && text.contains("| [mcp_servers.app]"), "{text}");
    // a long file is cut in the preview
    let long = Setup { skills: vec![Skill::new("app", "d", &"line\n".repeat(40))], ..setup("/x/app") };
    let text = install(Agent::Claude, &Scope::User, &long, &host).unwrap().describe(true);
    assert!(text.contains("more lines") && text.matches("| line").count() < 12, "{text}");
    assert!(snapshot(dir.path()).is_empty(), "nothing written");
}

#[test]
fn detects_agents_by_folder_program_or_app() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::at(dir.path(), Os::Mac);
    assert!(detect(&host).iter().all(|d| !d.found));
    std::fs::create_dir_all(dir.path().join(".codex")).unwrap();
    std::fs::create_dir_all(dir.path().join("Applications/Cursor.app")).unwrap();
    std::fs::create_dir_all(dir.path().join("Applications/Claude.app")).unwrap();
    let found: Vec<Agent> = detect(&host).into_iter().filter(|d| d.found).map(|d| d.agent).collect();
    assert_eq!(found, [Agent::Codex, Agent::Cursor], "the Claude desktop app is not Claude Code");
}

#[test]
fn config_dir_overrides_are_honoured() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = Host::at(dir.path().join("home"), Os::Mac);
    host.vars.insert("CLAUDE_CONFIG_DIR".into(), dir.path().join("claude").display().to_string());
    host.vars.insert("CODEX_HOME".into(), dir.path().join("codex").display().to_string());
    let s = setup("/x/app");
    install(Agent::Claude, &Scope::User, &s, &host).unwrap().apply("app").unwrap();
    install(Agent::Codex, &Scope::User, &s, &host).unwrap().apply("app").unwrap();
    assert!(dir.path().join("claude/.claude.json").is_file() && dir.path().join("claude/skills/app/SKILL.md").is_file());
    assert!(dir.path().join("codex/config.toml").is_file());
    // only the shared skills folder (Codex's skills) is in the home folder
    let in_home: Vec<String> = snapshot(&dir.path().join("home")).into_iter().map(|(p, _)| p).collect();
    assert!(in_home.iter().all(|p| p.starts_with(".agents/skills/")), "{in_home:?}");
}

#[test]
fn select_run_and_report() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::at(dir.path(), Os::Mac);
    assert!(select(None, &host).unwrap_err().contains("no coding agent"));
    std::fs::create_dir_all(dir.path().join(".gemini")).unwrap();
    assert_eq!(select(None, &host).unwrap(), [Agent::Gemini]);
    assert_eq!(select(Some("claude, code,claude"), &host).unwrap(), [Agent::Claude, Agent::VsCode]);
    assert_eq!(select(Some("all"), &host).unwrap().len(), 6);
    assert!(select(Some("emacs"), &host).unwrap_err().contains("unknown agent `emacs`"));
    let s = setup("/x/app");
    let dry = run(Op::Install(&s), &[Agent::Gemini], &Scope::User, &host, false, "app");
    assert!(report(&dry, false, false).contains("(nothing written: --print)"));
    assert!(!dir.path().join(".gemini/settings.json").exists());
    let done = run(Op::Install(&s), &[Agent::Gemini], &Scope::User, &host, true, "app");
    assert_eq!(done[0].applied.len(), 3, "{done:?}");
    assert!(report(&run(Op::Install(&s), &[Agent::Gemini], &Scope::User, &host, true, "app"), true, false).contains("Gemini CLI: up to date"));
    let gone = run(Op::Uninstall("app"), &[Agent::Gemini], &Scope::User, &host, true, "app");
    assert_eq!(gone[0].applied.iter().filter(|a| a.action == Action::Remove).count(), 2);
}

#[test]
fn overview_lists_every_agent_with_its_state() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::at(dir.path(), Os::Mac);
    std::fs::create_dir_all(dir.path().join(".cursor")).unwrap();
    let s = setup("/x/app");
    install(Agent::Cursor, &Scope::User, &s, &host).unwrap().apply("app").unwrap();
    let o = overview(&s, &Scope::User, &host);
    assert_eq!(o.len(), 6);
    let cursor = o.iter().find(|a| a.agent == Agent::Cursor).unwrap();
    assert!(cursor.found && cursor.state == Some(State::Current));
    let claude = o.iter().find(|a| a.agent == Agent::Claude).unwrap();
    assert!(!claude.found && claude.state == Some(State::Missing));
    assert_eq!(serde_json::to_value(cursor).unwrap()["state"], "current");
}

#[test]
fn two_runs_in_one_second_keep_both_backups_and_skills_folders_go() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::at(dir.path(), Os::Mac);
    write(&dir.path().join(".claude.json"), OTHER_JSON);
    let s = setup("/x/app");
    install(Agent::Claude, &Scope::User, &s, &host).unwrap().apply("app").unwrap();
    uninstall(Agent::Claude, &Scope::User, "app", &host).unwrap().apply("app").unwrap();
    let backups: Vec<(String, String)> = snapshot(dir.path()).into_iter().filter(|(p, _)| p.contains("-backup-")).collect();
    assert_eq!(backups.len(), 2, "{backups:?}");
    assert!(backups.iter().any(|(_, t)| t == OTHER_JSON), "the first backup holds the original");
    assert!(!dir.path().join(".claude/skills").exists());
    // a re-run reports what is already in place in one line
    install(Agent::Claude, &Scope::User, &s, &host).unwrap().apply("app").unwrap();
    let mut s2 = s.clone();
    s2.server.command = "/y/app".into();
    let text = install(Agent::Claude, &Scope::User, &s2, &host).unwrap().describe(false);
    assert!(text.contains("update") && text.contains("(2 files already up to date)") && !text.contains("unchanged"), "{text}");
}
