# Security policy

## Supported versions

ashlar is in early development. Security fixes go into the latest released minor version and
into `main`.

| Version | Supported |
|---|---|
| 0.1.x (latest) | yes |
| older | no |

## Reporting a vulnerability

**Please don't report security vulnerabilities in public issues, discussions or pull requests.**

Report them privately through GitHub:
[open a private security advisory](https://github.com/Himasnhu-AT/ashlar/security/advisories/new).

Please include:

- the affected crate(s) and version or commit
- a description of the issue and its impact
- steps to reproduce, or a proof of concept
- any suggested fix

You can expect an acknowledgement within a few days. We'll keep you updated while we work on a
fix, and credit you in the advisory unless you'd rather stay anonymous. Please give us reasonable
time to release a fix before you disclose the issue publicly.

## Scope

Areas where security matters most in ashlar:

- the agent control port (`ashlar-control`) and MCP over HTTP (`ashlar-mcp-http`): authentication,
  Host and Origin checks, input limits
- downloading and verifying integrations (`ashlar-integrations`) and update checks (`ashlar-update`)
- telemetry (`ashlar-trace`, `ashlar-trace-server`): data scrubbing, ingest limits, the dashboard
- parsing of untrusted documents and files

Dependencies are checked against the RustSec advisory database on every CI run.
