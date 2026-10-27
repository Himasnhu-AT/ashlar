//! [`Integrations`]: the app's handle. Catalog, installs, servers and tools behind one thread-safe API
//! that never blocks the caller on the network or a child process: work runs on background threads
//! (one job per integration at a time), and [`Integrations::snapshot`] tells the UI where each one is.

use std::collections::BTreeMap;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::catalog::{Entry, MAX_CATALOG, Manifest, expand, parse_catalog, url_ok};
use crate::download::{DownloadError, Http, get_small};
use crate::links::Links;
use crate::mcp::ServerCommand;
use crate::server::{Server, ServerConfig, ServerStatus};
use crate::store::{self, InstallError, Installed, Store, assets_digest, missing_assets};
use crate::tool::{CallOutput, Prompt, Resource, SEPARATOR, Tool, qualified_name};
use crate::verify::Verify;

/// How many links one install may ask for (each one resumes the download).
const LINK_ROUNDS: usize = 4;
/// Default time a server gets to answer the handshake.
pub const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
/// Default time a tool call may take.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(600);

/// How the app sets the manager up.
#[derive(Debug, Clone)]
pub struct Config {
    /// `<app data dir>/integrations`.
    pub root: PathBuf,
    /// The catalog's URL (`https://…/v1/integrations/<app>?platform=…&arch=…`); `None` shows only what is installed.
    pub catalog_url: Option<String>,
    /// `macos`.
    pub platform: String,
    /// `arm64` or `x86_64`.
    pub arch: String,
    /// The app's name and version (the MCP client info; the version is compared with `min_app_version`).
    pub app_name: String,
    pub app_version: String,
    /// The Apple team id bundles must be signed with (checked against a bundle manifest's own claim too).
    pub team_id: Option<String>,
}

impl Config {
    /// The platform and architecture this build runs on.
    pub fn this_machine() -> (String, String) {
        let platform = std::env::consts::OS;
        let arch = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            other => other,
        };
        (platform.to_string(), arch.to_string())
    }
}

/// A background job's progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Job {
    Downloading { done: u64, total: u64 },
    Verifying,
    DownloadingAssets { done: u64, total: u64 },
    Starting,
    Removing,
}

/// The catalog's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CatalogState {
    NotLoaded,
    Loading,
    Loaded,
    Failed { message: String },
}

/// One integration, for the UI.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub licence: String,
    pub homepage: Option<String>,
    pub notes: Option<String>,
    /// The catalog's version, when the catalog lists it.
    pub available: Option<u32>,
    /// The zip's size.
    pub download_size: u64,
    /// Bytes downloaded on first use.
    pub models_size: Option<u64>,
    pub installed: Option<u32>,
    pub installed_label: Option<String>,
    pub previous: Option<u32>,
    pub enabled: bool,
    pub update_available: bool,
    /// The app is new enough for the catalog's build.
    pub compatible: bool,
    /// The first-use download (models) still has to happen.
    pub needs_assets: bool,
    pub job: Option<Job>,
    pub last_error: Option<String>,
    pub server: ServerStatus,
    /// The tools the assistant sees (qualified names).
    pub tools: Vec<String>,
}

/// Everything, for the UI.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Snapshot {
    pub catalog: CatalogState,
    pub items: Vec<Item>,
}

/// A tool as the assistant sees it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostTool {
    /// `<id>__<tool>`.
    pub name: String,
    pub integration: String,
    pub tool: Tool,
}

/// A prompt as a command: `<id>:<prompt>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostPrompt {
    /// `<id>:<prompt>` ([`PROMPT_SEPARATOR`]).
    pub name: String,
    pub integration: String,
    pub prompt: Prompt,
}

/// A resource an integration offers as a skill (`skill://<name>`), named `<id>:<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostSkillResource {
    pub name: String,
    pub integration: String,
    pub resource: Resource,
}

/// Between an integration's id and its prompt's or skill's name in a command (`earshot:transcribe`).
pub const PROMPT_SEPARATOR: char = ':';

/// The folder of skills in a bundle when its manifest names none.
pub const DEFAULT_SKILLS_DIR: &str = "skills";

type Waker = Arc<dyn Fn() + Send + Sync>;

struct Shared {
    config: Config,
    http: Arc<dyn Http>,
    verify: Arc<dyn Verify>,
    store: Mutex<Store>,
    catalog: Mutex<(CatalogState, Vec<Entry>)>,
    servers: Mutex<BTreeMap<String, Arc<Server>>>,
    jobs: Mutex<BTreeMap<String, (Job, Arc<AtomicBool>)>>,
    errors: Mutex<BTreeMap<String, String>>,
    /// Manifests of installed versions, read once.
    manifests: Mutex<BTreeMap<(String, u32), Manifest>>,
    links: Mutex<Option<Arc<dyn Links>>>,
    waker: Waker,
    closed: AtomicBool,
}

/// The app's integrations. Cheap to clone; dropping the last clone doesn't stop the servers: call
/// [`Integrations::shutdown`] when the app quits.
#[derive(Clone)]
pub struct Integrations {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Integrations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Integrations").field("root", &self.shared.config.root).finish_non_exhaustive()
    }
}

/// Why a request was refused at once (work that starts reports failures in [`Item::last_error`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    #[error("there's no integration {0:?}")]
    Unknown(String),
    #[error("{0} is busy")]
    Busy(String),
    #[error("{0} isn't installed")]
    NotInstalled(String),
    #[error("{0}")]
    Other(String),
}

impl Integrations {
    /// Open the store, show the cached catalog, and start every enabled integration in the background.
    /// `waker` is called (from any thread) whenever the snapshot changes.
    pub fn new(config: Config, http: Arc<dyn Http>, verify: Arc<dyn Verify>, waker: impl Fn() + Send + Sync + 'static) -> Integrations {
        let store = Store::open(&config.root);
        let cached =
            fs::read(config.root.join("catalog.json")).ok().and_then(|b| parse_catalog(&b, &config.platform, &config.arch).ok()).unwrap_or_default();
        let shared = Arc::new(Shared {
            config,
            http,
            verify,
            store: Mutex::new(store),
            catalog: Mutex::new((CatalogState::NotLoaded, cached)),
            servers: Mutex::new(BTreeMap::new()),
            jobs: Mutex::new(BTreeMap::new()),
            errors: Mutex::new(BTreeMap::new()),
            manifests: Mutex::new(BTreeMap::new()),
            links: Mutex::new(None),
            waker: Arc::new(waker),
            closed: AtomicBool::new(false),
        });
        let me = Integrations { shared };
        let enabled: Vec<String> = {
            let store = me.shared.store.lock().unwrap_or_else(PoisonError::into_inner);
            store.ids().into_iter().filter(|id| store.get(id).is_some_and(|s| s.enabled && s.current.is_some())).collect()
        };
        for id in enabled {
            let _ = me.start_in_background(&id);
        }
        me
    }

    pub fn root(&self) -> &Path {
        &self.shared.config.root
    }

    /// Fetch the catalog in the background.
    pub fn refresh_catalog(&self) {
        let Some(url) = self.shared.config.catalog_url.clone() else { return };
        {
            let mut c = self.shared.catalog.lock().unwrap_or_else(PoisonError::into_inner);
            if c.0 == CatalogState::Loading {
                return;
            }
            c.0 = CatalogState::Loading;
        }
        self.wake();
        let shared = Arc::clone(&self.shared);
        spawn("integrations-catalog", move || {
            let result = if url_ok(&url) {
                get_small(shared.http.as_ref(), &url, MAX_CATALOG).and_then(|(status, body)| {
                    if status != 200 {
                        return Err(format!("the catalog answered HTTP {status}"));
                    }
                    let list = parse_catalog(&body, &shared.config.platform, &shared.config.arch).map_err(|e| e.0)?;
                    let _ = fs::create_dir_all(&shared.config.root);
                    let _ = ashlar_fs::atomic_write(&shared.config.root.join("catalog.json"), &body);
                    Ok(list)
                })
            } else {
                Err("the catalog URL must be https://".into())
            };
            let mut c = shared.catalog.lock().unwrap_or_else(PoisonError::into_inner);
            match result {
                Ok(list) => *c = (CatalogState::Loaded, list),
                Err(message) => {
                    log::warn!(target: "integrations", "catalog: {message}");
                    c.0 = CatalogState::Failed { message };
                }
            }
            drop(c);
            (shared.waker)();
        });
    }

    /// Replace the catalog (tests, or an app that gets it some other way).
    pub fn set_catalog(&self, entries: Vec<Entry>) {
        *self.shared.catalog.lock().unwrap_or_else(PoisonError::into_inner) = (CatalogState::Loaded, entries);
        self.wake();
    }

    fn wake(&self) {
        (self.shared.waker)();
    }

    fn catalog_entry(&self, id: &str) -> Option<Entry> {
        self.shared.catalog.lock().unwrap_or_else(PoisonError::into_inner).1.iter().find(|e| e.id == id).cloned()
    }

    /// Install the catalog's build of `id` (or update to it), then start it.
    pub fn install(&self, id: &str) -> Result<(), Refused> {
        let entry = self.catalog_entry(id).ok_or_else(|| Refused::Unknown(id.to_string()))?;
        if !version_at_least(&self.shared.config.app_version, &entry.min_app_version) {
            return Err(Refused::Other(format!("{} needs app version {} or later", entry.title, entry.min_app_version)));
        }
        if entry.url.is_empty() && self.links().is_none() {
            return Err(Refused::Other(format!("{} can't be downloaded by this build (it has no account to ask for a link)", entry.title)));
        }
        let cancel = self.begin(id, Job::Downloading { done: 0, total: entry.size })?;
        let me = self.clone();
        let id = id.to_string();
        spawn(&format!("integrations-install-{id}"), move || {
            let result = me.do_install(&entry, &cancel);
            me.finish(&id, result.map_err(|e| e.to_string()));
        });
        Ok(())
    }

    /// Where install links come from, when the catalog doesn't publish them.
    pub fn set_links(&self, links: Arc<dyn Links>) {
        *self.shared.links.lock().unwrap_or_else(PoisonError::into_inner) = Some(links);
    }

    fn links(&self) -> Option<Arc<dyn Links>> {
        self.shared.links.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn do_install(&self, listed: &Entry, cancel: &AtomicBool) -> Result<(), InstallError> {
        let id = listed.id.clone();
        let progress = |done: u64| self.set_job(&id, Job::Downloading { done, total: listed.size });
        let root = self.shared.config.root.clone();
        let links = self.links();
        // Download and check without holding the store: other integrations keep working meanwhile. A
        // signed link that runs out mid-download (403) is asked for again and the download resumes.
        let mut entry = listed.clone();
        let mut prepared = None;
        for round in 0..LINK_ROUNDS {
            if entry.url.is_empty() || round > 0 {
                let Some(links) = &links else { break };
                let link = links.link(listed).map_err(|e| InstallError::Link(e.to_string()))?;
                entry.url = link.url;
                entry.mirror = link.mirror;
            }
            let verify = Arc::clone(&self.shared.verify);
            let http = Arc::clone(&self.shared.http);
            let team = self.shared.config.team_id.clone();
            let progress_then_verify = |done: u64| {
                progress(done);
                if done >= entry.size {
                    self.set_job(&id, Job::Verifying);
                }
            };
            match store::prepare(&root, &entry, http.as_ref(), verify.as_ref(), team.as_deref(), &progress_then_verify, cancel) {
                Err(InstallError::Download(DownloadError::Failed { detail, .. }))
                    if links.is_some() && detail.contains("HTTP 403") && round + 1 < LINK_ROUNDS =>
                {
                    log::info!(target: "integrations", "[{id}] the download link ran out; asking for a new one");
                }
                result => {
                    prepared = Some(result?);
                    break;
                }
            }
        }
        let Some(mut prepared) = prepared else {
            return Err(InstallError::Link("no download link".into()));
        };
        // Signed links don't outlive the install.
        prepared.entry.url.clear();
        prepared.entry.mirror = None;
        // Swap versions with the server stopped.
        self.stop_server(&id);
        self.shared.store.lock().unwrap_or_else(PoisonError::into_inner).commit(prepared)?;
        self.prepare_and_start(&id, cancel)
    }

    /// Download the first-use files when needed, then start the server.
    fn prepare_and_start(&self, id: &str, cancel: &AtomicBool) -> Result<(), InstallError> {
        let (installed, manifest, data, ready) = {
            let store = self.shared.store.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(saved) = store.get(id) else { return Err(InstallError::Disk(format!("{id} isn't installed"))) };
            let Some(installed) = saved.current.clone() else { return Err(InstallError::Disk(format!("{id} isn't installed"))) };
            if !saved.enabled {
                return Ok(());
            }
            let manifest = store.manifest(&installed)?;
            (installed, manifest, store.data_dir(id), saved.assets_ready.clone())
        };
        let digest = assets_digest(&manifest);
        if !manifest.assets.is_empty() && (ready.as_deref() != Some(digest.as_str()) || !missing_assets(&manifest, &data).is_empty()) {
            let total = manifest.assets_size();
            self.set_job(id, Job::DownloadingAssets { done: 0, total });
            store::fetch_assets(
                &manifest,
                &data,
                self.shared.http.as_ref(),
                &|done, total| self.set_job(id, Job::DownloadingAssets { done, total }),
                cancel,
            )?;
            self.shared.store.lock().unwrap_or_else(PoisonError::into_inner).set_assets_ready(id, Some(digest))?;
        }
        self.set_job(id, Job::Starting);
        let server = Arc::new(Server::new(self.server_config(&installed, &manifest, &data)));
        let old = self.shared.servers.lock().unwrap_or_else(PoisonError::into_inner).insert(id.to_string(), Arc::clone(&server));
        if let Some(old) = old {
            old.stop();
        }
        self.wake();
        server.start().map_err(|e| InstallError::Disk(format!("{id} didn't start: {e}")))
    }

    fn server_config(&self, installed: &Installed, manifest: &Manifest, data: &Path) -> ServerConfig {
        let bundle = self.shared.config.root.join(&installed.entry.id).join(installed.version.to_string());
        let program = bundle.join(&installed.entry.entry);
        let args = installed.entry.args.iter().map(|a| expand(a, data, &bundle)).collect();
        let mut env: Vec<(String, String)> = vec![("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into())];
        for k in ["HOME", "USER", "LOGNAME", "LANG", "LC_ALL"] {
            if let Ok(v) = std::env::var(k) {
                env.push((k.into(), v));
            }
        }
        env.push(("TMPDIR".into(), std::env::temp_dir().to_string_lossy().into_owned()));
        for (k, v) in &manifest.env {
            env.push((k.clone(), expand(v, data, &bundle)));
        }
        let secs = |s: Option<u64>, d: Duration| s.map(|s| Duration::from_secs(s.clamp(1, 24 * 3600))).unwrap_or(d);
        ServerConfig {
            id: installed.entry.id.clone(),
            command: ServerCommand { program, args, env, cwd: Some(data.to_path_buf()), label: installed.entry.id.clone() },
            startup_timeout: secs(manifest.startup_timeout_secs, DEFAULT_STARTUP_TIMEOUT),
            call_timeout: secs(manifest.call_timeout_secs, DEFAULT_CALL_TIMEOUT),
            hidden: manifest.hidden_tools.clone(),
            client_name: self.shared.config.app_name.clone(),
            client_version: self.shared.config.app_version.clone(),
        }
    }

    fn start_in_background(&self, id: &str) -> Result<(), Refused> {
        let cancel = self.begin(id, Job::Starting)?;
        let me = self.clone();
        let id = id.to_string();
        let _ = fs::create_dir_all(self.shared.config.root.join(&id).join("data"));
        spawn(&format!("integrations-start-{id}"), move || {
            let result = me.prepare_and_start(&id, &cancel);
            me.finish(&id, result.map_err(|e| e.to_string()));
        });
        Ok(())
    }

    /// Turn `id` on (and start it) or off (and stop it).
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), Refused> {
        self.shared
            .store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_enabled(id, enabled)
            .map_err(|_| Refused::NotInstalled(id.to_string()))?;
        if enabled {
            self.start_in_background(id)?;
        } else {
            self.stop_server(id);
            self.shared.servers.lock().unwrap_or_else(PoisonError::into_inner).remove(id);
        }
        self.wake();
        Ok(())
    }

    /// Restart a server (after it gave up restarting itself).
    pub fn restart(&self, id: &str) -> Result<(), Refused> {
        self.stop_server(id);
        self.shared.servers.lock().unwrap_or_else(PoisonError::into_inner).remove(id);
        self.start_in_background(id)
    }

    /// Go back to the previous installed version.
    pub fn rollback(&self, id: &str) -> Result<u32, Refused> {
        if self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner).contains_key(id) {
            return Err(Refused::Busy(id.to_string()));
        }
        self.stop_server(id);
        let v = self.shared.store.lock().unwrap_or_else(PoisonError::into_inner).rollback(id).map_err(|e| Refused::Other(e.to_string()))?;
        self.start_in_background(id)?;
        Ok(v)
    }

    /// Stop `id` and delete it with its data.
    pub fn remove(&self, id: &str) -> Result<(), Refused> {
        if self.shared.store.lock().unwrap_or_else(PoisonError::into_inner).get(id).is_none() {
            return Err(Refused::NotInstalled(id.to_string()));
        }
        let _cancel = self.begin(id, Job::Removing)?;
        let me = self.clone();
        let id = id.to_string();
        spawn(&format!("integrations-remove-{id}"), move || {
            me.stop_server(&id);
            me.shared.servers.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
            let result = me.shared.store.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
            me.finish(&id, result.map_err(|e| e.to_string()));
        });
        Ok(())
    }

    /// Cancel `id`'s running job (a download).
    pub fn cancel(&self, id: &str) {
        if let Some((_, flag)) = self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner).get(id) {
            flag.store(true, Ordering::SeqCst);
        }
    }

    fn stop_server(&self, id: &str) {
        let server = self.shared.servers.lock().unwrap_or_else(PoisonError::into_inner).get(id).cloned();
        if let Some(s) = server {
            s.stop();
        }
    }

    fn begin(&self, id: &str, job: Job) -> Result<Arc<AtomicBool>, Refused> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(Refused::Other("shutting down".into()));
        }
        let mut jobs = self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        if jobs.contains_key(id) {
            return Err(Refused::Busy(id.to_string()));
        }
        let flag = Arc::new(AtomicBool::new(false));
        jobs.insert(id.to_string(), (job, Arc::clone(&flag)));
        drop(jobs);
        self.shared.errors.lock().unwrap_or_else(PoisonError::into_inner).remove(id);
        self.wake();
        Ok(flag)
    }

    fn set_job(&self, id: &str, job: Job) {
        let mut jobs = self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = jobs.get_mut(id) {
            slot.0 = job;
        }
        drop(jobs);
        self.wake();
    }

    fn finish(&self, id: &str, result: Result<(), String>) {
        self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner).remove(id);
        if let Err(e) = result {
            log::warn!(target: "integrations", "[{id}] {e}");
            self.shared.errors.lock().unwrap_or_else(PoisonError::into_inner).insert(id.to_string(), e);
        }
        self.wake();
    }

    /// Where everything is.
    pub fn snapshot(&self) -> Snapshot {
        let (catalog_state, catalog) = self.shared.catalog.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let store = self.shared.store.lock().unwrap_or_else(PoisonError::into_inner);
        let servers = self.shared.servers.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let jobs = self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let errors = self.shared.errors.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let mut ids: Vec<String> = catalog.iter().map(|e| e.id.clone()).collect();
        for id in store.ids() {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let items = ids
            .into_iter()
            .filter_map(|id| {
                let listed = catalog.iter().find(|e| e.id == id);
                let saved = store.get(&id);
                let current = saved.and_then(|s| s.current.as_ref());
                let entry = listed.or(current.map(|c| &c.entry))?;
                let server = servers.get(&id);
                let needs_assets = match current {
                    Some(c) => self
                        .manifest_of(&store, c)
                        .is_some_and(|m| !m.assets.is_empty() && saved.and_then(|s| s.assets_ready.clone()) != Some(assets_digest(&m))),
                    None => entry.models_size.is_some_and(|n| n > 0),
                };
                Some(Item {
                    id: id.clone(),
                    title: entry.title.clone(),
                    summary: entry.summary.clone(),
                    licence: entry.licence.clone(),
                    homepage: entry.homepage.clone(),
                    notes: entry.notes.clone(),
                    available: listed.map(|e| e.version),
                    download_size: entry.size,
                    models_size: entry.models_size,
                    installed: current.map(|c| c.version),
                    installed_label: current.map(|c| c.display_version.clone()).filter(|s| !s.is_empty()),
                    previous: saved.and_then(|s| s.previous.as_ref()).map(|p| p.version),
                    enabled: saved.is_some_and(|s| s.enabled),
                    update_available: matches!((listed, current), (Some(l), Some(c)) if l.version > c.version),
                    compatible: listed.is_none_or(|l| version_at_least(&self.shared.config.app_version, &l.min_app_version)),
                    needs_assets,
                    job: jobs.get(&id).map(|j| j.0.clone()),
                    last_error: errors.get(&id).cloned(),
                    server: server.map(|s| s.status()).unwrap_or(ServerStatus::Stopped),
                    tools: server.map(|s| s.tools().iter().map(|t| qualified_name(&id, &t.name)).collect()).unwrap_or_default(),
                })
            })
            .collect();
        Snapshot { catalog: catalog_state, items }
    }

    fn manifest_of(&self, store: &Store, installed: &Installed) -> Option<Manifest> {
        let key = (installed.entry.id.clone(), installed.version);
        let mut cache = self.shared.manifests.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(m) = cache.get(&key) {
            return Some(m.clone());
        }
        let m = store.manifest(installed).ok()?;
        cache.insert(key, m.clone());
        Some(m)
    }

    /// The tools of every running integration, sorted by name (a stable order keeps prompt caches warm).
    pub fn tools(&self) -> Vec<HostTool> {
        let servers = self.shared.servers.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let mut out: Vec<HostTool> = servers
            .iter()
            .flat_map(|(id, s)| s.tools().into_iter().map(move |t| HostTool { name: qualified_name(id, &t.name), integration: id.clone(), tool: t }))
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Servers that said their prompts or resources changed list them again in the background
    /// (the snapshot's waker runs when they have).
    fn refresh_changed_extras(&self, servers: &BTreeMap<String, Arc<Server>>) {
        for (id, s) in servers {
            if s.take_extras_changed() {
                let (me, s) = (self.clone(), Arc::clone(s));
                spawn(&format!("integrations-extras-{id}"), move || {
                    if let Err(e) = s.refresh_extras() {
                        log::warn!(target: "integrations", "[{}] listing prompts and resources again: {e}", s.id());
                    }
                    me.wake();
                });
            }
        }
    }

    /// The prompts of every running integration as commands, sorted by name. A server that said
    /// they changed lists them again in the background.
    pub fn prompts(&self) -> Vec<HostPrompt> {
        let servers = self.shared.servers.lock().unwrap_or_else(PoisonError::into_inner).clone();
        self.refresh_changed_extras(&servers);
        let mut out: Vec<HostPrompt> = servers
            .iter()
            .flat_map(|(id, s)| {
                s.prompts().into_iter().map(move |p| HostPrompt {
                    name: format!("{id}{PROMPT_SEPARATOR}{}", p.name),
                    integration: id.clone(),
                    prompt: p,
                })
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// A prompt's text (`name` as [`Self::prompts`] gives it) with `typed` (what followed the
    /// command) as its arguments ([`Prompt::arguments_from`]). Blocks: call it off the UI thread.
    pub fn get_prompt(&self, name: &str, typed: &str) -> Result<String, String> {
        let (id, prompt) = name.split_once(PROMPT_SEPARATOR).ok_or_else(|| format!("{name:?} isn't an integration's prompt"))?;
        let server =
            self.shared.servers.lock().unwrap_or_else(PoisonError::into_inner).get(id).cloned().ok_or_else(|| format!("{id} isn't running"))?;
        let p = server.prompts().into_iter().find(|p| p.name == prompt).ok_or_else(|| format!("{id} has no prompt {prompt:?}"))?;
        let missing: Vec<&str> =
            p.arguments.iter().filter(|a| a.required).map(|a| a.name.as_str()).filter(|n| !p.arguments_from(typed).contains_key(*n)).collect();
        if !missing.is_empty() {
            return Err(format!("/{name} needs {}", missing.join(", ")));
        }
        server.get_prompt(prompt, &p.arguments_from(typed))
    }

    /// The skill folders of the enabled, installed integrations: `(id, folder)`, each folder
    /// holding skill folders (`<name>/SKILL.md`): the manifest's `skills`, or `skills`, relative
    /// to the folder that holds `integration.json`. Only folders inside the bundle.
    pub fn skill_dirs(&self) -> Vec<(String, PathBuf)> {
        let store = self.shared.store.lock().unwrap_or_else(PoisonError::into_inner);
        let mut out = Vec::new();
        for id in store.ids() {
            let Some(saved) = store.get(&id).filter(|s| s.enabled) else { continue };
            let Some(current) = saved.current.as_ref() else { continue };
            let bundle = store.version_dir(&id, current.version).join(&id);
            let listed = self.manifest_of(&store, current).map(|m| m.skills).unwrap_or_default();
            let dirs: Vec<String> = if listed.is_empty() { vec![DEFAULT_SKILLS_DIR.to_string()] } else { listed };
            for d in dirs {
                let dir = bundle.join(&d);
                if dir.is_dir() && dir.canonicalize().ok().zip(bundle.canonicalize().ok()).is_some_and(|(d, b)| d.starts_with(b)) {
                    out.push((id.clone(), dir));
                }
            }
        }
        out
    }

    /// The resources running integrations offer as skills, sorted by name (refreshed like
    /// [`Self::prompts`]).
    pub fn skill_resources(&self) -> Vec<HostSkillResource> {
        let servers = self.shared.servers.lock().unwrap_or_else(PoisonError::into_inner).clone();
        self.refresh_changed_extras(&servers);
        let mut out: Vec<HostSkillResource> = servers
            .iter()
            .flat_map(|(id, s)| {
                s.resources().into_iter().filter_map(move |r| {
                    let n = r.skill_name()?.to_string();
                    Some(HostSkillResource { name: format!("{id}{PROMPT_SEPARATOR}{n}"), integration: id.clone(), resource: r })
                })
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// A resource's text from integration `id`. Blocks: call it off the UI thread.
    pub fn read_resource(&self, id: &str, uri: &str) -> Result<String, String> {
        let server =
            self.shared.servers.lock().unwrap_or_else(PoisonError::into_inner).get(id).cloned().ok_or_else(|| format!("{id} isn't running"))?;
        server.read_resource(uri)
    }

    /// Whether `name` (a qualified name) belongs to an integration.
    pub fn owns(&self, name: &str) -> bool {
        name.contains(SEPARATOR) && self.find(name).is_some()
    }

    fn find(&self, name: &str) -> Option<(Arc<Server>, Tool)> {
        let servers = self.shared.servers.lock().unwrap_or_else(PoisonError::into_inner).clone();
        servers.into_iter().find_map(|(id, s)| s.tools().into_iter().find(|t| qualified_name(&id, &t.name) == name).map(|t| (s, t)))
    }

    /// Why a call needs the person's approval (`None` for tools that only read and stay on this machine).
    pub fn needs_approval(&self, name: &str) -> Option<String> {
        let (server, tool) = self.find(name)?;
        if tool.read_only && !tool.open_world {
            return None;
        }
        let what = if tool.open_world { "may send data off this machine" } else { "may create or change files" };
        Some(format!("{} ({}) {what}.", tool.title.clone().unwrap_or_else(|| tool.name.clone()), server.id()))
    }

    /// Call a tool by its qualified name. Blocks until it answers, times out or `cancel` is set: call it
    /// from a worker thread.
    pub fn call(&self, name: &str, input: &Value, cancel: Option<&AtomicBool>) -> CallOutput {
        match self.find(name) {
            Some((server, tool)) => server.call(&tool.name, input, cancel),
            None => CallOutput::error(format!("there's no tool {name:?} (its integration may have stopped)")),
        }
    }

    /// Stop every server and refuse new jobs (the app is quitting). Running downloads are cancelled.
    pub fn shutdown(&self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        for (_, flag) in self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner).values() {
            flag.store(true, Ordering::SeqCst);
        }
        let servers = std::mem::take(&mut *self.shared.servers.lock().unwrap_or_else(PoisonError::into_inner));
        for s in servers.values() {
            s.stop();
        }
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    let name = name.to_string();
    let label = name.clone();
    let spawned = thread::Builder::new().name(name).spawn(move || {
        if catch_unwind(AssertUnwindSafe(f)).is_err() {
            log::error!(target: "integrations", "{label} panicked");
        }
    });
    if let Err(e) = spawned {
        log::error!(target: "integrations", "couldn't start a thread: {e}");
    }
}

/// `have >= want` comparing dot-separated numbers (`0.10.0` > `0.9.2`); an empty `want` always passes,
/// anything after `-` or `+` is ignored.
pub fn version_at_least(have: &str, want: &str) -> bool {
    let parse =
        |s: &str| -> Vec<u64> { s.split(['-', '+']).next().unwrap_or_default().split('.').map(|p| p.trim().parse::<u64>().unwrap_or(0)).collect() };
    if want.trim().is_empty() {
        return true;
    }
    let (h, w) = (parse(have), parse(want));
    for i in 0..h.len().max(w.len()) {
        let (a, b) = (h.get(i).copied().unwrap_or(0), w.get(i).copied().unwrap_or(0));
        if a != b {
            return a > b;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(version_at_least("0.10.0", "0.9.2"));
        assert!(version_at_least("1.0.0", "1.0"));
        assert!(version_at_least("1.2.3-beta", "1.2.3"));
        assert!(!version_at_least("0.4.9", "0.5.0"));
        assert!(version_at_least("0.1.0", ""));
    }
}
