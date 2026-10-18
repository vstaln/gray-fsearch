//! Wire-contract e2e: the argv manifest and the wire manifest must agree,
//! requests must get exactly one reply, notifications stay silent, and
//! plugin/shutdown / stdin EOF must exit promptly.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_gray-fsearch")
}

struct Sidecar {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
}

fn spawn_sidecar() -> Sidecar {
    let mut child = Command::new(bin())
        .arg("sidecar")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sidecar");
    let stdin = child.stdin.take().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    Sidecar {
        child,
        stdin: Some(stdin),
        stdout,
    }
}

impl Sidecar {
    fn send(&mut self, v: serde_json::Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }

    fn read(&mut self) -> serde_json::Value {
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).unwrap();
        assert!(n > 0, "sidecar closed stdout without replying");
        serde_json::from_str(&line).expect("reply is not JSON")
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_dead(sc: &mut Sidecar, why: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(Some(status)) = sc.child.try_wait() {
            assert!(status.success(), "{why}: exited {status}");
            return;
        }
        assert!(Instant::now() < deadline, "{why}: still running after 5s");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn argv_and_wire_manifest_agree() {
    let out = Command::new(bin())
        .arg("manifest")
        .output()
        .expect("manifest argv");
    assert!(out.status.success(), "manifest argv: {:?}", out.status);
    let argv_manifest: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    let mut sc = spawn_sidecar();
    sc.send(serde_json::json!({"id": 1, "method": "plugin/manifest"}));
    let reply = sc.read();
    assert_eq!(reply["id"], 1);
    assert_eq!(
        reply["result"], argv_manifest,
        "wire manifest != argv manifest"
    );
    assert_eq!(argv_manifest["name"], "fsearch");
    assert_eq!(argv_manifest["protocol"], "1.1");
    assert!(
        argv_manifest["shell_env"]["vars"]["GRAY_SEARCH_SOCK"]
            .as_str()
            .is_some_and(|s| s.ends_with("search.sock") && s.starts_with('/')),
        "shell_env must carry an absolute socket path"
    );
}

#[test]
fn requests_replied_notifications_silent() {
    let mut sc = spawn_sidecar();
    // Unknown methods must get a -32601, not silence.
    sc.send(serde_json::json!({"id": 7, "method": "nope/method"}));
    let reply = sc.read();
    assert_eq!(reply["id"], 7);
    assert_eq!(reply["error"]["code"], -32601);
    // A claimed hook must answer (we claim prompt/context for spawn-at-start).
    sc.send(serde_json::json!({"id": 8, "method": "prompt/context",
        "params": {"session": {"id": "t", "cwd": "/"}}}));
    assert_eq!(sc.read()["id"], 8);
    // A notification (no id) must produce no reply — the next reply we
    // read must belong to the following request, not the notification.
    sc.send(serde_json::json!({"method": "event/notify", "params": {"type": "test"}}));
    sc.send(serde_json::json!({"id": 9, "method": "plugin/manifest"}));
    let reply = sc.read();
    assert_eq!(
        reply["id"], 9,
        "notification leaked a reply or a reply was dropped"
    );
}

#[test]
fn shutdown_and_eof_exit() {
    let mut sc = spawn_sidecar();
    sc.send(serde_json::json!({"method": "plugin/shutdown", "params": {"reason": "test"}}));
    wait_dead(&mut sc, "plugin/shutdown");

    let mut sc = spawn_sidecar();
    drop(sc.stdin.take()); // stdin EOF = host died → exit promptly
    wait_dead(&mut sc, "stdin EOF");
}
