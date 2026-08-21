use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

const TOKEN: &str = "integration-token";
const PROTOCOL: &str = "2026-07-28";

struct Daemon {
    child: Child,
    port: u16,
    client: reqwest::Client,
}

impl Daemon {
    async fn start(cwd: &Path, tools: &[String]) -> Self {
        let mut args = vec!["--cwd".to_string(), cwd.display().to_string()];
        for tool in tools {
            args.push("--tool".to_string());
            args.push(tool.clone());
        }
        Self::launch(&args).await
    }

    async fn with_config(config: &Path) -> Self {
        Self::launch(&["-c".to_string(), config.display().to_string()]).await
    }

    async fn launch(args: &[String]) -> Self {
        Self::launch_with(args, true).await
    }

    async fn launch_with(args: &[String], token: bool) -> Self {
        let port = free_port();

        let mut command = Command::new(env!("CARGO_BIN_EXE_mcpd"));
        command
            .arg("--bind")
            .arg(format!("127.0.0.1:{port}"))
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        if token {
            command.env("MCPD_TOKEN", TOKEN);
        } else {
            command.env_remove("MCPD_TOKEN");
        }

        let daemon = Self {
            child: command.spawn().expect("spawn mcpd"),
            port,
            client: reqwest::Client::new(),
        };
        daemon.await_health(args).await;
        daemon
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    async fn await_health(&self, args: &[String]) {
        for _ in 0..200 {
            if let Ok(response) = self.client.get(self.url("/health")).send().await
                && response.status().is_success()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("mcpd never became healthy with {args:?}");
    }

    /// Every request carries the 2026-07-28 protocol metadata: the standard
    /// headers from SEP-2243 and the per-request `_meta` the stateless mode
    /// requires.
    async fn rpc(&self, method: &str, name: &str, mut params: Value) -> Value {
        params.as_object_mut().expect("params object").insert(
            "_meta".to_string(),
            json!({
                "io.modelcontextprotocol/protocolVersion": PROTOCOL,
                "io.modelcontextprotocol/clientCapabilities": {},
            }),
        );

        let mut request = self
            .client
            .post(self.url("/mcp"))
            .bearer_auth(TOKEN)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL)
            .header("Mcp-Method", method);

        if !name.is_empty() {
            request = request.header("Mcp-Name", name);
        }

        request
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": method,
                "params": params,
            }))
            .send()
            .await
            .expect("send")
            .json()
            .await
            .expect("decode")
    }

    async fn call(&self, tool: &str, arguments: Value) -> Value {
        self.rpc(
            "tools/call",
            tool,
            json!({ "name": tool, "arguments": arguments }),
        )
        .await
    }

    fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    fn terminate(&mut self) -> Duration {
        let started = Instant::now();
        unsafe { libc::kill(self.pid(), libc::SIGTERM) };
        for _ in 0..600 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return started.elapsed();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("mcpd did not exit after SIGTERM");
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

fn sh(timeout_ms: u64, max_output_bytes: usize) -> String {
    json!({
        "name": "sh",
        "description": "Run a shell command.",
        "inputSchema": {
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"],
        },
        "_meta": {
            "dev.subs/exec": {
                "argv": ["/bin/sh", "-lc", "{command}"],
                "timeoutMs": timeout_ms,
                "maxOutputBytes": max_output_bytes,
            },
        },
    })
    .to_string()
}

fn lines() -> String {
    json!({
        "name": "lines",
        "inputSchema": {
            "type": "object",
            "properties": { "paths": { "type": "array", "items": { "type": "string" } } },
            "required": ["paths"],
        },
        "_meta": {
            "dev.subs/exec": { "argv": ["/usr/bin/printf", "%s\n", "{paths}"] },
        },
    })
    .to_string()
}

fn text(response: &Value) -> String {
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text content in {response}"))
        .to_string()
}

fn is_error(response: &Value) -> bool {
    response["result"]["isError"].as_bool().unwrap_or(false)
}

async fn shell_daemon(cwd: &Path) -> Daemon {
    Daemon::start(cwd, &[sh(2_000, 50_000), lines()]).await
}

#[tokio::test]
async fn health_is_served_without_a_token() {
    let dir = TempDir::new().unwrap();
    let daemon = shell_daemon(dir.path()).await;

    let response = reqwest::get(daemon.url("/health")).await.unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.text().await.unwrap(), env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn mcp_without_a_token_is_rejected_with_an_empty_body() {
    let dir = TempDir::new().unwrap();
    let daemon = shell_daemon(dir.path()).await;

    let response = reqwest::Client::new()
        .post(daemon.url("/mcp"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 401);
    assert!(response.text().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_wrong_token_is_rejected() {
    let dir = TempDir::new().unwrap();
    let daemon = shell_daemon(dir.path()).await;

    let response = reqwest::Client::new()
        .post(daemon.url("/mcp"))
        .bearer_auth("not-the-token")
        .json(&json!({}))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn the_server_advertises_only_the_pinned_protocol_version() {
    let dir = TempDir::new().unwrap();
    let daemon = shell_daemon(dir.path()).await;

    let response = daemon
        .rpc(
            "initialize",
            "",
            json!({
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" },
            }),
        )
        .await;

    assert_eq!(response["result"]["protocolVersion"], PROTOCOL);
    assert_eq!(response["result"]["serverInfo"]["name"], "mcpd");
}

#[tokio::test]
async fn tools_are_listed_without_their_exec_details() {
    let dir = TempDir::new().unwrap();
    let daemon = shell_daemon(dir.path()).await;

    let response = daemon.rpc("tools/list", "", json!({})).await;
    let tools = response["result"]["tools"].as_array().unwrap();

    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["name"], "sh");
    assert_eq!(tools[0]["description"], "Run a shell command.");
    assert!(tools.iter().all(|tool| tool.get("_meta").is_none()));
}

#[tokio::test]
async fn a_command_runs_and_returns_its_output() {
    let dir = TempDir::new().unwrap();
    let daemon = shell_daemon(dir.path()).await;

    let response = daemon.call("sh", json!({ "command": "echo hi" })).await;

    assert!(!is_error(&response));
    assert_eq!(text(&response), "hi\n");
}

#[tokio::test]
async fn a_command_runs_in_the_configured_working_directory() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("marker.txt"), "x").unwrap();
    let daemon = shell_daemon(dir.path()).await;

    let response = daemon.call("sh", json!({ "command": "ls" })).await;

    assert_eq!(text(&response), "marker.txt\n");
}

#[tokio::test]
async fn a_non_zero_exit_is_a_successful_call() {
    let dir = TempDir::new().unwrap();
    let daemon = shell_daemon(dir.path()).await;

    let response = daemon
        .call("sh", json!({ "command": "echo out; echo bad >&2; exit 3" }))
        .await;

    assert!(!is_error(&response));
    assert_eq!(text(&response), "out\nbad\n[exit code 3]");
}

#[tokio::test]
async fn an_argument_reaches_the_command_without_a_shell_in_between() {
    let dir = TempDir::new().unwrap();
    let daemon = Daemon::start(dir.path(), &[lines()]).await;

    let response = daemon
        .call("lines", json!({ "paths": ["a b", "$HOME", "c;rm"] }))
        .await;

    assert_eq!(text(&response), "a b\n$HOME\nc;rm\n");
}

#[tokio::test]
async fn a_timeout_is_an_error_that_still_reports_its_output() {
    let dir = TempDir::new().unwrap();
    let daemon = Daemon::start(dir.path(), &[sh(500, 50_000)]).await;

    let response = daemon
        .call("sh", json!({ "command": "echo progress; sleep 30" }))
        .await;

    assert!(is_error(&response));
    let text = text(&response);
    assert!(text.contains("timed out after 500ms"), "{text}");
    assert!(text.contains("exit code 124"), "{text}");
    assert!(text.contains("progress"), "{text}");
}

#[tokio::test]
async fn a_timeout_takes_the_whole_process_group() {
    let dir = TempDir::new().unwrap();
    let marker = dir.path().join("escaped");
    let daemon = Daemon::start(dir.path(), &[sh(500, 50_000)]).await;

    daemon
        .call(
            "sh",
            json!({ "command": format!("(sleep 2 && touch {}) & sleep 30", marker.display()) }),
        )
        .await;

    tokio::time::sleep(Duration::from_millis(3_000)).await;
    assert!(!marker.exists(), "a grandchild outlived the timeout");
}

#[tokio::test]
async fn output_beyond_the_limit_is_truncated_at_both_ends() {
    let dir = TempDir::new().unwrap();
    let daemon = Daemon::start(dir.path(), &[sh(5_000, 200)]).await;

    let response = daemon
        .call(
            "sh",
            json!({ "command": "yes 0123456789 | head -c 200000" }),
        )
        .await;

    assert!(!is_error(&response));
    let text = text(&response);
    assert!(text.contains("bytes elided"), "{text}");
    assert!(text.len() < 400, "kept {} bytes", text.len());
}

#[tokio::test]
async fn an_unknown_tool_is_a_protocol_error() {
    let dir = TempDir::new().unwrap();
    let daemon = shell_daemon(dir.path()).await;

    let response = daemon.call("nope", json!({})).await;

    assert_eq!(response["error"]["code"], -32602);
}

#[tokio::test]
async fn a_schema_violation_is_a_tool_error_not_a_protocol_error() {
    let dir = TempDir::new().unwrap();
    let daemon = shell_daemon(dir.path()).await;

    let response = daemon.call("sh", json!({ "command": 7 })).await;

    assert!(response.get("error").is_none(), "{response}");
    assert!(is_error(&response));
}

#[tokio::test]
async fn a_request_without_the_protocol_metadata_is_rejected() {
    let dir = TempDir::new().unwrap();
    let daemon = shell_daemon(dir.path()).await;

    let response: Value = reqwest::Client::new()
        .post(daemon.url("/mcp"))
        .bearer_auth(TOKEN)
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {},
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(response.get("error").is_some(), "{response}");
}

#[tokio::test]
async fn no_auth_serves_every_caller_including_one_with_no_credentials() {
    let dir = TempDir::new().unwrap();
    let daemon = Daemon::launch_with(
        &[
            "--no-auth".to_string(),
            "--cwd".to_string(),
            dir.path().display().to_string(),
            "--tool".to_string(),
            sh(2_000, 50_000),
        ],
        false,
    )
    .await;

    let unauthenticated: Value = reqwest::Client::new()
        .post(daemon.url("/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", PROTOCOL)
        .header("Mcp-Method", "tools/call")
        .header("Mcp-Name", "sh")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "sh",
                "arguments": { "command": "echo open" },
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": PROTOCOL,
                    "io.modelcontextprotocol/clientCapabilities": {},
                },
            },
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(text(&unauthenticated), "open\n");
}

#[tokio::test]
async fn no_auth_alongside_a_token_is_refused_as_ambiguous() {
    let output = Command::new(env!("CARGO_BIN_EXE_mcpd"))
        .arg("--bind")
        .arg("127.0.0.1:0")
        .arg("--no-auth")
        .env("MCPD_TOKEN", TOKEN)
        .output()
        .expect("run mcpd");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--no-auth"), "{stderr}");
    assert!(stderr.contains("MCPD_TOKEN"), "{stderr}");
}

#[tokio::test]
async fn a_config_file_cannot_turn_authentication_off() {
    let dir = TempDir::new().unwrap();
    let config = dir.path().join("mcpd.toml");
    std::fs::write(&config, "no-auth = true\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_mcpd"))
        .arg("-c")
        .arg(&config)
        .env_remove("MCPD_TOKEN")
        .output()
        .expect("run mcpd");

    assert!(!output.status.success());
}

#[tokio::test]
async fn the_mcp_endpoint_can_be_moved_to_another_path() {
    let dir = TempDir::new().unwrap();
    let daemon = Daemon::launch(&[
        "--mcp-path".to_string(),
        "/agent/mcp".to_string(),
        "--cwd".to_string(),
        dir.path().display().to_string(),
        "--tool".to_string(),
        sh(2_000, 50_000),
    ])
    .await;

    let moved: Value = daemon
        .client
        .post(daemon.url("/agent/mcp"))
        .bearer_auth(TOKEN)
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", PROTOCOL)
        .header("Mcp-Method", "tools/list")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": PROTOCOL,
                    "io.modelcontextprotocol/clientCapabilities": {},
                },
            },
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(moved["result"]["tools"][0]["name"], "sh");

    let old = daemon
        .client
        .post(daemon.url("/mcp"))
        .bearer_auth(TOKEN)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(old.status(), 404);

    let health = reqwest::get(daemon.url("/health")).await.unwrap();
    assert!(health.status().is_success());
}

#[tokio::test]
async fn an_mcp_path_that_collides_with_the_health_check_is_refused() {
    let output = Command::new(env!("CARGO_BIN_EXE_mcpd"))
        .arg("--bind")
        .arg("127.0.0.1:0")
        .arg("--mcp-path")
        .arg("/health")
        .env("MCPD_TOKEN", TOKEN)
        .output()
        .expect("run mcpd");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("health"), "{stderr}");
}

#[tokio::test]
async fn the_daemon_refuses_to_start_without_a_token() {
    let output = Command::new(env!("CARGO_BIN_EXE_mcpd"))
        .arg("--bind")
        .arg("127.0.0.1:0")
        .env_remove("MCPD_TOKEN")
        .output()
        .expect("run mcpd");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("MCPD_TOKEN"));
}

#[tokio::test]
async fn an_empty_token_is_not_a_token() {
    let output = Command::new(env!("CARGO_BIN_EXE_mcpd"))
        .arg("--bind")
        .arg("127.0.0.1:0")
        .env("MCPD_TOKEN", "")
        .output()
        .expect("run mcpd");

    assert!(!output.status.success());
}

#[tokio::test]
async fn the_daemon_token_never_reaches_a_command() {
    let dir = TempDir::new().unwrap();
    let daemon = shell_daemon(dir.path()).await;

    let response = daemon
        .call(
            "sh",
            json!({ "command": "env | grep -c '^MCPD_TOKEN=' || true" }),
        )
        .await;

    assert_eq!(text(&response).trim(), "0");
}

#[tokio::test]
async fn a_config_file_configures_the_whole_server() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("marker.txt"), "x").unwrap();

    let config = dir.path().join("mcpd.toml");
    std::fs::write(
        &config,
        format!(
            r#"
bind = "127.0.0.1:1"
name = "configured"
cwd = "{cwd}"
instructions = "prefer scoped test runs"
list-ttl-ms = 1234

[[tool]]
name = "sh"
description = "Run a shell command."

[tool.inputSchema]
type = "object"
required = ["command"]

[tool.inputSchema.properties.command]
type = "string"

[tool._meta."dev.subs/exec"]
argv = ["/bin/sh", "-lc", "{{command}}"]
timeoutMs = 2000
"#,
            cwd = dir.path().display()
        ),
    )
    .unwrap();

    // `bind` in the file is deliberately unusable: the daemon came up on the
    // port from the flag, so the flag won.
    let daemon = Daemon::with_config(&config).await;

    let initialized = daemon
        .rpc(
            "initialize",
            "",
            json!({
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" },
            }),
        )
        .await;
    assert_eq!(initialized["result"]["serverInfo"]["name"], "configured");
    assert_eq!(
        initialized["result"]["instructions"],
        "prefer scoped test runs"
    );

    let listed = daemon.rpc("tools/list", "", json!({})).await;
    assert_eq!(listed["result"]["tools"][0]["name"], "sh");
    assert_eq!(
        listed["result"]["tools"][0]["description"],
        "Run a shell command."
    );
    assert!(listed["result"]["tools"][0].get("_meta").is_none());
    assert_eq!(listed["result"]["ttlMs"], 1234);
    assert_eq!(listed["result"]["cacheScope"], "public");

    let response = daemon.call("sh", json!({ "command": "ls" })).await;
    assert_eq!(text(&response), "marker.txt\nmcpd.toml\n");
}

#[tokio::test]
async fn every_shipped_example_starts_and_serves_its_tools() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut checked = 0;

    for entry in std::fs::read_dir(&examples).expect("examples directory") {
        let path = entry.unwrap().path();
        if !matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("toml" | "json")
        ) {
            continue;
        }

        let daemon = Daemon::with_config(&path).await;
        let listed = daemon.rpc("tools/list", "", json!({})).await;
        let tools = listed["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("{} listed no tools: {listed}", path.display()));

        assert!(!tools.is_empty(), "{} serves nothing", path.display());
        assert!(
            tools.iter().all(|tool| tool.get("_meta").is_none()),
            "{} leaks exec details",
            path.display()
        );
        checked += 1;
    }

    assert!(checked >= 2, "expected example configs, found {checked}");
}

#[tokio::test]
async fn a_malformed_config_file_stops_the_daemon_starting() {
    let dir = TempDir::new().unwrap();
    let config = dir.path().join("mcpd.toml");
    std::fs::write(&config, "bnid = \"0.0.0.0:8080\"\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_mcpd"))
        .arg("--config")
        .arg(&config)
        .env("MCPD_TOKEN", TOKEN)
        .output()
        .expect("run mcpd");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("mcpd.toml"), "{stderr}");
}

#[tokio::test]
async fn sigterm_leaves_no_orphaned_process_group() {
    let dir = TempDir::new().unwrap();
    let marker = dir.path().join("orphan");
    let mut daemon = Daemon::start(dir.path(), &[sh(60_000, 50_000)]).await;

    let port = daemon.port;
    let command = format!("(sleep 3 && touch {}) & sleep 60", marker.display());
    tokio::spawn(async move {
        let _ = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/mcp"))
            .bearer_auth(TOKEN)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL)
            .header("Mcp-Method", "tools/call")
            .header("Mcp-Name", "sh")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "sh",
                    "arguments": { "command": command },
                    "_meta": {
                        "io.modelcontextprotocol/protocolVersion": PROTOCOL,
                        "io.modelcontextprotocol/clientCapabilities": {},
                    },
                },
            }))
            .send()
            .await;
    });

    tokio::time::sleep(Duration::from_millis(800)).await;

    let elapsed = daemon.terminate();
    assert!(
        elapsed < Duration::from_secs(10),
        "SIGTERM took {elapsed:?}; the blocking reader is still holding the runtime open"
    );

    tokio::time::sleep(Duration::from_millis(3_500)).await;
    assert!(
        !marker.exists(),
        "a command outlived the daemon it was spawned by"
    );
}
