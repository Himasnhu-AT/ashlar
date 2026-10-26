//! Starts the `ashlar-trace-server` binary as a child process on a data directory, so its memory can be measured
//! on its own.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Spawned {
    pub child: Child,
    pub url: String,
    pub key: String,
    pub db: PathBuf,
    pub dir: PathBuf,
    owns_dir: bool,
}

fn server_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("LOADGEN_SERVER_BIN") {
        return PathBuf::from(path);
    }
    let exe = std::env::current_exe().unwrap_or_default();
    exe.parent().map(|dir| dir.join("ashlar-trace-server")).unwrap_or_default()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map_or(18_099, |a| a.port())
}

fn base_command(dir: &Path, extra_env: &[(String, String)]) -> Command {
    let mut command = Command::new(server_binary());
    command
        .env("TRACER_DATA_DIR", dir)
        .env("TRACER_RATE_PER_MIN", "0")
        .env("TRACER_KEY_RATE_PER_MIN", "0")
        .env("TRACER_SECURE_COOKIES", "0")
        .env_remove("TRACER_ADMIN_PASSWORD");
    for (k, v) in extra_env {
        command.env(k, v);
    }
    command
}

/// Creates the app (when the directory has none) and starts `serve`. `dir` of `None` makes a temp directory
/// that is removed on drop.
pub fn spawn(dir: Option<PathBuf>, extra_env: &[(String, String)]) -> Result<Spawned, String> {
    let owns_dir = dir.is_none();
    let dir = dir.unwrap_or_else(|| std::env::temp_dir().join(format!("tracer-loadgen-{}", std::process::id())));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let listing = base_command(&dir, extra_env)
        .args(["app", "list"])
        .output()
        .map_err(|e| format!("cannot run {}: {e} (build ashlar-trace-server first)", server_binary().display()))?;
    let key = if String::from_utf8_lossy(&listing.stdout).contains("loadgen") {
        let out = base_command(&dir, extra_env).args(["key", "create", "loadgen"]).output().map_err(|e| e.to_string())?;
        String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.strip_prefix("write key: ").map(str::to_string))
    } else {
        let out = base_command(&dir, extra_env).args(["app", "create", "loadgen", "Load generator"]).output().map_err(|e| e.to_string())?;
        String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.strip_prefix("write key: ").map(str::to_string))
    }
    .ok_or("could not create a write key")?;
    let port = free_port();
    let child = base_command(&dir, extra_env)
        .env("TRACER_BIND", format!("127.0.0.1:{port}"))
        .arg("serve")
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let url = format!("http://127.0.0.1:{port}");
    let started = Instant::now();
    let agent = crate::run::agent(1);
    loop {
        if agent.get(format!("{url}/v1/health")).call().is_ok_and(|r| r.status().as_u16() == 200) {
            break;
        }
        if started.elapsed() > Duration::from_secs(20) {
            return Err("the server did not come up".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(Spawned { child, url, key, db: dir.join("tracer.db"), dir, owns_dir })
}

impl Spawned {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Asks the server to stop (SIGTERM; elsewhere it is killed) and waits for it.
    pub fn stop(&mut self) {
        #[cfg(unix)]
        let _ = Command::new("kill").arg(self.child.id().to_string()).status();
        #[cfg(not(unix))]
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Spawned {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if self.owns_dir {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}
