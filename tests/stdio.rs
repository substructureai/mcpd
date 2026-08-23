mod common;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

use common::{PROTOCOL, TOKEN, is_error, sh, text};

struct Piped {
    child: Child,
    input: Option<std::process::ChildStdin>,
    output: std::sync::mpsc::Receiver<String>,
    stderr: std::path::PathBuf,
}

impl Piped {
    fn start(dir: &Path, args: &[String], token: bool) -> Self {
        let stderr = dir.join("mcpd.stderr");

        let mut command = Command::new(env!("CARGO_BIN_EXE_mcpd"));
        command
            .arg("--stdio")
            .arg("--cwd")
            .arg(dir)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(&stderr).expect("stderr file"));

        if token {
            command.env("MCPD_TOKEN", TOKEN);
        } else {
            command.env_remove("MCPD_TOKEN");
        }

        let mut child = command.spawn().expect("spawn mcpd");
        let input = child.stdin.take();
        let mut lines = std::io::BufRead::lines(std::io::BufReader::new(
            child.stdout.take().expect("stdout"),
        ));

        let (sender, output) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            while let Some(Ok(line)) = lines.next() {
                if sender.send(line).is_err() {
                    return;
                }
            }
        });

        Self {
            child,
            input,
            output,
            stderr,
        }
    }

    fn send(&mut self, message: Value) {
        use std::io::Write;
        let input = self.input.as_mut().expect("stdin is still open");
        writeln!(input, "{message}").expect("write");
        input.flush().expect("flush");
    }

    fn receive(&mut self) -> Value {
        let line = self
            .output
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("mcpd sent nothing; stderr:\n{}", self.logs()));
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON-RPC: {line}: {e}"))
    }

    fn rpc(&mut self, id: u32, method: &str, params: Value) -> Value {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        self.receive()
    }

    fn initialize(&mut self, version: &str) -> Value {
        let response = self.rpc(
            1,
            "initialize",
            json!({
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" },
            }),
        );
        self.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        response
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.rpc(
            3,
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        )
    }

    fn terminate(&mut self) -> Duration {
        let started = Instant::now();
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };

        for _ in 0..200 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return started.elapsed();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("mcpd did not exit after SIGTERM; stderr:\n{}", self.logs());
    }

    fn logs(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    fn close(&mut self) -> std::process::ExitStatus {
        self.input.take();

        for _ in 0..200 {
            if let Ok(Some(status)) = self.child.try_wait() {
                return status;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("mcpd outlived the client that closed its stdin");
    }
}

impl Drop for Piped {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn piped(dir: &Path) -> Piped {
    Piped::start(dir, &["--tool".to_string(), sh(2_000, 50_000)], false)
}

#[test]
fn stdio_serves_the_pinned_protocol_and_the_same_tools() {
    let dir = TempDir::new().unwrap();
    let mut daemon = piped(dir.path());

    let initialized = daemon.initialize(PROTOCOL);
    assert_eq!(initialized["result"]["protocolVersion"], PROTOCOL);
    assert_eq!(initialized["result"]["serverInfo"]["name"], "mcpd");

    let listed = daemon.rpc(2, "tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "sh");
    assert!(tools[0].get("_meta").is_none());

    let called = daemon.call("sh", json!({ "command": "echo hi" }));
    assert_eq!(text(&called), "hi\n");
    assert!(!is_error(&called));
}

#[test]
fn stdio_asks_for_no_token_and_no_waiver() {
    let dir = TempDir::new().unwrap();
    let mut daemon = piped(dir.path());

    assert!(daemon.initialize(PROTOCOL)["result"].is_object());
}

#[test]
fn an_older_revision_is_answered_with_the_only_one_served() {
    let dir = TempDir::new().unwrap();
    let mut daemon = piped(dir.path());

    let initialized = daemon.initialize("2025-06-18");

    assert_eq!(initialized["result"]["protocolVersion"], PROTOCOL);
}

#[test]
fn the_cache_hints_come_with_the_revision_that_defines_them() {
    let dir = TempDir::new().unwrap();
    let mut daemon = piped(dir.path());

    daemon.initialize(PROTOCOL);

    let listed = daemon.rpc(2, "tools/list", json!({}));
    assert_eq!(listed["result"]["ttlMs"], 60_000);
    assert_eq!(listed["result"]["cacheScope"], "public");
}

#[test]
fn a_version_nobody_recognises_falls_back_to_the_one_served() {
    let dir = TempDir::new().unwrap();
    let mut daemon = piped(dir.path());

    let initialized = daemon.initialize("1999-01-01");

    assert_eq!(initialized["result"]["protocolVersion"], PROTOCOL);
}

#[test]
fn stdio_keeps_everything_but_the_protocol_off_stdout() {
    let dir = TempDir::new().unwrap();
    let mut daemon = piped(dir.path());

    daemon.initialize(PROTOCOL);
    let listed = daemon.rpc(2, "tools/list", json!({}));
    assert_eq!(listed["jsonrpc"], "2.0");

    let logs = daemon.logs();
    assert!(logs.contains("serving on stdin and stdout"), "{logs}");
    assert!(logs.contains("registered"), "{logs}");
    assert!(
        !logs.contains('\u{1b}'),
        "logs carry terminal escapes: {logs:?}"
    );
}

#[test]
fn the_daemon_token_never_reaches_a_command_over_stdio() {
    let dir = TempDir::new().unwrap();
    let mut daemon = Piped::start(dir.path(), &["--tool".to_string(), sh(2_000, 50_000)], true);

    daemon.initialize(PROTOCOL);
    let called = daemon.call(
        "sh",
        json!({ "command": "env | grep -c '^MCPD_TOKEN=' || true" }),
    );

    assert_eq!(text(&called).trim(), "0");
}

#[test]
fn closing_stdin_ends_the_session() {
    let dir = TempDir::new().unwrap();
    let mut daemon = piped(dir.path());
    daemon.initialize(PROTOCOL);

    assert!(daemon.close().success());
}

#[test]
fn a_flag_that_only_means_something_over_http_is_refused_with_stdio() {
    let dir = TempDir::new().unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_mcpd"))
        .args(["--stdio", "--bind", "127.0.0.1:9"])
        .arg("--cwd")
        .arg(dir.path())
        .env_remove("MCPD_TOKEN")
        .output()
        .expect("run mcpd");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--bind"), "{stderr}");
    assert!(stderr.contains("--stdio"), "{stderr}");
}

#[test]
fn a_config_file_that_binds_a_port_is_still_servable_over_stdio() {
    let dir = TempDir::new().unwrap();
    let mut daemon = Piped::start(
        dir.path(),
        &["-c".to_string(), "examples/minimal.toml".to_string()],
        false,
    );

    daemon.initialize(PROTOCOL);
    let listed = daemon.rpc(2, "tools/list", json!({}));

    assert_eq!(listed["result"]["tools"][0]["name"], "bash");
}

#[test]
fn sigterm_ends_a_stdio_session_and_leaves_no_orphan() {
    let dir = TempDir::new().unwrap();
    let marker = dir.path().join("orphan");
    let mut daemon = Piped::start(
        dir.path(),
        &["--tool".to_string(), sh(60_000, 50_000)],
        false,
    );

    daemon.initialize(PROTOCOL);
    daemon.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "sh",
            "arguments": {
                "command": format!("(sleep 3 && touch {}) & sleep 60", marker.display()),
            },
        },
    }));

    std::thread::sleep(Duration::from_millis(800));

    let elapsed = daemon.terminate();
    assert!(
        elapsed < Duration::from_secs(10),
        "SIGTERM took {elapsed:?}; something is still holding the runtime open"
    );

    std::thread::sleep(Duration::from_millis(3_500));
    assert!(
        !marker.exists(),
        "a command outlived the daemon it was spawned by"
    );
}
