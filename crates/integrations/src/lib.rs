//! Integrations: tool servers an app downloads from a catalog and runs as MCP servers over stdio,
//! their tools offered to an in-app assistant.
//!
//! An integration is a zip of one folder holding a code-signed executable that speaks MCP on stdio.
//! The app fetches a catalog ([`catalog::parse_catalog`]), and on request [`Integrations`] downloads a
//! build (resumable, size and sha256 checked), unpacks it into
//! `<root>/<id>/<version>/`, checks the code signature against the expected team before anything in
//! it runs, downloads the files its manifest asks for on first use (models) into `<root>/<id>/data/`,
//! and starts it. One previous version is kept for [`Integrations::rollback`].
//!
//! | Piece | What it does |
//! |---|---|
//! | [`Integrations`] | the app's handle: catalog, install/update/remove, enable/disable, [`Snapshot`] for the UI, tools and calls for the assistant |
//! | [`catalog`] | the catalog's entries and the bundle's `integration.json` manifest, validated |
//! | [`download`] | resumable, verified downloads over an [`Http`] trait ([`HttpsClient`] with feature `https`) |
//! | [`links`] | download links asked for at install time, signed for the signed-in account ([`SignedLinks`], [`Integrations::set_links`]) |
//! | [`verify`] | the code-signature check ([`Codesign`]) |
//! | [`mcp`] | the MCP stdio client: handshake, `tools/list`, `tools/call`, `prompts/*`, `resources/*`, timeouts, cancellation |
//! | [`server`] | one supervised server: start, restart after a crash, cached tool list |
//!
//! Besides tools, an integration can bring the assistant skills and commands: its MCP prompts become
//! `/<id>:<prompt>` commands ([`Integrations::prompts`], [`Integrations::get_prompt`]), resources at
//! `skill://<name>` and the skill folders in its bundle (the manifest's `skills`, or `skills/`) become
//! skills named `<id>:<name>` ([`Integrations::skill_resources`], [`Integrations::skill_dirs`]).
//!
//! The assistant sees each tool as `<id>__<tool>` ([`qualified_name`]): provider-safe (`[A-Za-z0-9_-]`,
//! at most 64 characters) and routed by its prefix. A tool needs approval unless its server marks it
//! read-only and not open-world ([`Integrations::needs_approval`]).
//!
//! Servers get a clean environment (a minimal `PATH`, `HOME`, `TMPDIR`, the locale and what the
//! manifest adds), their stderr goes to the log (target `integrations`), and they are stopped on
//! [`Integrations::shutdown`] (stdin closed, then killed). A server also exits by itself when the app
//! dies, since its stdin closes.

pub mod catalog;
pub mod download;
pub mod links;
pub mod mcp;
pub mod server;
mod service;
pub mod store;
pub mod tool;
pub mod verify;

pub use catalog::{Asset, Entry, Manifest};
#[cfg(all(feature = "https", not(target_arch = "wasm32")))]
pub use download::HttpsClient;
pub use download::{Http, HttpResponse};
pub use links::{Link, LinkError, Links, SignedLinks, Tokens};
pub use server::ServerStatus;
pub use service::{
    CatalogState, Config, DEFAULT_CALL_TIMEOUT, DEFAULT_SKILLS_DIR, DEFAULT_STARTUP_TIMEOUT, HostPrompt, HostSkillResource, HostTool, Integrations,
    Item, Job, PROMPT_SEPARATOR, Refused, Snapshot, version_at_least,
};
pub use tool::{CallOutput, Content, Prompt, PromptArg, Resource, SKILL_SCHEME, Tool, integration_of, qualified_name};
#[cfg(any(test, feature = "fake-server"))]
pub use verify::AcceptAll;
pub use verify::{Codesign, Verify};
