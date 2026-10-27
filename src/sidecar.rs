//! Plugin sidecar: NDJSON wire loop on stdio plus the search socket.
//!
//! On startup the sidecar lays down the PATH shims
//! (~/.gray/fsearch/bin/{grep,rg} → this binary) and binds the fixed
//! search socket at ~/.gray/fsearch/search.sock, serving it from the
//! session root (git worktree of the launch cwd, else cwd). Exactly one
//! live sidecar serves, chosen by an exclusive flock on serve.lock; the
//! rest poll for it and take over when the owning session exits, so shims
//! never fall back to full-tree walks just because the first session quit.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::paths::gray_home;

pub fn base_dir() -> Option<PathBuf> {
    gray_home().map(|h| h.join("fsearch"))
}

pub fn socket_path() -> Option<PathBuf> {
    base_dir().map(|d| d.join("search.sock"))
}

pub fn bin_dir() -> Option<PathBuf> {
    base_dir().map(|d| d.join("bin"))
}

/// The plugin manifest — identical bytes on `fsearch manifest` (install
/// probe / `gray plugin check`) and the `plugin/manifest` wire reply.
pub fn manifest_json() -> Value {
    let sock = socket_path().unwrap_or_else(|| PathBuf::from(".gray/fsearch/search.sock"));
    let bin = bin_dir().unwrap_or_else(|| PathBuf::from(".gray/fsearch/bin"));
    json!({
        "name": "fsearch",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": "1.1",
        "tools": [],
        "commands": [],
        // Earliest hook the host fires each session (prompt/context runs
        // during the first turn's prompt build, before agent/before_start
        // and any tool/before); LazySidecarPlugin spawns the sidecar on
        // the first claimed call, so this claim is our session-start
        // trigger. We answer it with {} (no context injected).
        "hooks": ["prompt/context"],
        "capabilities": ["shell.env"],
        "shell_env": {
            "vars": { "GRAY_SEARCH_SOCK": sock.to_string_lossy() },
            "path_prepend": [bin.to_string_lossy()],
        },
    })
}

/// Create ~/.gray/fsearch/bin/{grep,rg} symlinks to this binary so the
/// manifest's path_prepend puts our argv0 shims on the shell's PATH.
/// Best-effort: a failure only loses the fast path, the real tools run.
fn ensure_shim_dir() {
    let Some(bin) = bin_dir() else { return };
    if let Err(e) = std::fs::create_dir_all(&bin) {
        eprintln!("fsearch: cannot create {}: {e}", bin.display());
        return;
    }
    let Ok(me) = std::env::current_exe() else {
        return;
    };
    for tool in ["grep", "rg"] {
        let link = bin.join(tool);
        let ok = std::fs::symlink_metadata(&link)
            .and_then(|_| std::fs::read_link(&link))
            .is_ok_and(|t| t == me);
        if !ok {
            let _ = std::fs::remove_file(&link);
            if let Err(e) = std::os::unix::fs::symlink(&me, &link) {
                eprintln!("fsearch: cannot link {}: {e}", link.display());
            }
        }
    }
}

/// Is a server already answering on `sock`? Connect-only: a dead owner
/// leaves a file connect() refuses, a live one accepts (our empty request
/// then EOFs that one connection's handler harmlessly).
#[cfg(unix)]
pub fn socket_alive(sock: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(sock).is_ok()
}

/// Project root the socket server indexes/watches: the git (or jj)
/// worktree of the sidecar's cwd, else the cwd itself.
fn session_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    cwd.ancestors()
        .find(|p| p.join(".git").exists() || p.join(".jj").exists())
        .unwrap_or(&cwd)
        .to_path_buf()
}

/// ~/.gray/fsearch reused in place — unless it would sit inside the
/// indexed root (serve refuses that: the walker would emit index files as
/// candidates). Rare edge (cwd = $HOME or /); fall back to a tempdir.
#[cfg(target_os = "linux")]
fn pick_index_dir(root: &Path) -> Option<PathBuf> {
    let base = base_dir()?;
    let inside = base
        .canonicalize()
        .unwrap_or_else(|_| base.clone())
        .starts_with(root);
    (!inside).then_some(base)
}

/// How often a non-serving sidecar retries for the server role.
#[cfg(target_os = "linux")]
const TAKEOVER_POLL: std::time::Duration = std::time::Duration::from_secs(3);

/// Keep exactly one live sidecar serving the search socket.
///
/// The server role is an exclusive flock on ~/.gray/fsearch/serve.lock, not
/// a connect() probe: the kernel drops the lock when the owning session
/// exits, so another live sidecar takes over within one poll instead of
/// every shim silently falling back to a full-tree walk until a new session
/// starts. Holding the lock also stops two sidecars racing remove+bind on
/// the socket path.
#[cfg(target_os = "linux")]
fn spawn_server() {
    let (Some(sock), Some(base)) = (socket_path(), base_dir()) else {
        return;
    };
    let root = session_root();
    let index_dir = pick_index_dir(&root);
    std::thread::spawn(move || {
        let _ = std::fs::create_dir_all(&base);
        let lock_path = base.join("serve.lock");
        let mut reported = false;
        loop {
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&lock_path);
            // A live server without the lock is a pre-serve.lock sidecar
            // mid-upgrade: leave it be rather than stealing its socket.
            if let Ok(lock) = lock
                && lock.try_lock().is_ok()
                && !socket_alive(&sock)
            {
                // serve() only returns on failure; the lock drops with
                // `lock` at the end of this block so a peer can try.
                if let Err(e) = crate::search_serve::serve(&root, &sock, index_dir.as_deref())
                    && !reported
                {
                    eprintln!("fsearch: search server on {} failed: {e}", sock.display());
                    reported = true;
                }
            }
            std::thread::sleep(TAKEOVER_POLL);
        }
    });
}

#[cfg(not(target_os = "linux"))]
fn spawn_server() {}

fn write_reply(out: &mut impl Write, v: Value) -> std::io::Result<()> {
    writeln!(out, "{v}")?;
    out.flush()
}

/// NDJSON plugin wire loop (see ~/grayplugins/PLUGIN-CONTRACT.md):
/// requests get exactly one reply, notifications none, plugin/shutdown
/// exits promptly, EOF exits.
pub fn run() -> anyhow::Result<()> {
    ensure_shim_dir();
    spawn_server();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = v.get("method").and_then(Value::as_str).unwrap_or("");
        if method == "plugin/shutdown" {
            return Ok(());
        }
        let Some(id) = v.get("id").cloned() else {
            continue;
        };
        let reply = match method {
            "plugin/manifest" => json!({"id": id, "result": manifest_json()}),
            "prompt/context" => json!({"id": id, "result": {}}),
            m => json!({"id": id, "error":
                {"code": -32601, "message": format!("method not found: {m}")}}),
        };
        write_reply(&mut out, reply)?;
    }
    Ok(())
}
