# ashlar-agents

Connects a user's coding agents to an app. The app's MCP server goes into each agent's own config
file, and its skills (made from a category of the app's docs) go into the skills folder the agent
reads: `~/.claude/skills` for Claude Code, and the shared `~/.agents/skills` for Codex, Cursor,
Copilot in VS Code, Gemini CLI and Devin Desktop.

- `detect` says which agents are installed on this machine.
- `install` and `uninstall` return a `Plan`: every file they would create, change or remove, with
  the text, and nothing written yet. `Plan::apply` writes it, backing up each file it changes first.
- Re-running is idempotent. Edits merge into the user's files without touching their other entries,
  and uninstalling removes exactly what an install added.

Part of [ashlar](https://github.com/Himasnhu-AT/ashlar), a lightweight application framework for Rust powered by egui.
Licensed under the [BSD 3-Clause License](https://github.com/Himasnhu-AT/ashlar/blob/main/LICENSE).
