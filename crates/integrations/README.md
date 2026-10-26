# ashlar-integrations

Tool servers an app downloads from a catalog and runs as MCP servers over stdio, with their tools
offered to an in-app assistant.

An integration is a zip of one folder holding a code-signed executable that speaks MCP on stdio.
`Integrations` downloads a build (resumable, size and sha256 checked), unpacks it, checks the code
signature against the expected team before anything in it runs, and supervises the server
(restart after a crash, cached tool list). One previous version is kept for rollback.

| Module | What it does |
|---|---|
| `catalog` | the catalog's entries and each bundle's `integration.json` manifest, validated |
| `download` | resumable, verified downloads (`HttpsClient` with feature `https`) |
| `links` | download links asked for at install time, signed for the signed-in account |
| `verify` | the code-signature check |
| `mcp` | the MCP stdio client: handshake, `tools/list`, `tools/call`, timeouts, cancellation |
| `server` | one supervised server |

Part of [ashlar](https://github.com/Himasnhu-AT/ashlar), a lightweight application framework for Rust powered by egui.
Licensed under the [BSD 3-Clause License](https://github.com/Himasnhu-AT/ashlar/blob/main/LICENSE).
