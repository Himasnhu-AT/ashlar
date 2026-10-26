# ashlar-mcp-http

MCP over streamable HTTP on the loopback interface, for an app that lets local agents drive its
open window. `POST /mcp` carries one JSON-RPC message (or a batch); `DELETE /mcp` ends a session.

Every request must pass, in order: the path; a loopback `Host` and, when present, a loopback
`Origin` (against DNS rebinding); the bearer token, compared in constant time; and a `Gate` the app
can close while it must not be driven. The socket binds `127.0.0.1` only, and oversized bodies,
headers and connection floods are refused.

Part of [ashlar](https://github.com/Himasnhu-AT/ashlar), a lightweight application framework for Rust powered by egui.
Licensed under the [BSD 3-Clause License](https://github.com/Himasnhu-AT/ashlar/blob/main/LICENSE).
