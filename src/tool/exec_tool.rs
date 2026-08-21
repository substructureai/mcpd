use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool};

use crate::exec::{ExecOutput, ExecSpec, Executor, Exit, SIGNAL_EXIT_CODE_BASE, TIMEOUT_EXIT_CODE};
use crate::tool::{ToolDef, ToolError, ToolHandler, substitute};

pub struct ExecTool {
    def: ToolDef,
    executor: Arc<dyn Executor>,
    default_cwd: PathBuf,
}

impl ExecTool {
    pub fn new(def: ToolDef, executor: Arc<dyn Executor>, default_cwd: PathBuf) -> Self {
        Self {
            def,
            executor,
            default_cwd,
        }
    }

    /// `exec.cwd` → `--cwd` → inherited. A relative `exec.cwd` resolves against
    /// the daemon's directory rather than its own process cwd, so the same tool
    /// definition means the same thing wherever the daemon was launched from.
    fn cwd(&self, arguments: &JsonObject) -> Result<PathBuf, ToolError> {
        let Some(template) = self.def.exec.cwd.as_deref() else {
            return Ok(self.default_cwd.clone());
        };

        let resolved = substitute::text(template, arguments, self.def.schema.params())?;
        let path = Path::new(&resolved);

        Ok(if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.default_cwd.join(path)
        })
    }
}

#[async_trait]
impl ToolHandler for ExecTool {
    fn descriptor(&self) -> &Tool {
        &self.def.tool
    }

    async fn call(&self, arguments: Option<JsonObject>) -> Result<CallToolResult, ToolError> {
        let arguments = self.def.schema.prepare(arguments)?;
        let params = self.def.schema.params();

        // An array parameter spliced into argv can be empty, which would leave
        // nothing to run. Caught here rather than left to the executor.
        let argv = substitute::argv(&self.def.exec.argv, &arguments, params)?;
        if argv.is_empty() {
            return Err(ToolError::EmptyArgv);
        }

        let spec = ExecSpec {
            argv,
            cwd: self.cwd(&arguments)?,
            stdin: self
                .def
                .exec
                .stdin
                .as_deref()
                .map(|template| substitute::text(template, &arguments, params))
                .transpose()?,
            timeout: Duration::from_millis(self.def.exec.timeout_ms),
            max_output_bytes: self.def.exec.max_output_bytes,
        };

        let timeout = spec.timeout;
        Ok(render(self.executor.run(spec).await?, timeout))
    }
}

/// `isError` means the tool failed, not that the command exited non-zero.
/// `npm test` returning 1 is a successful call reporting a test failure, and a
/// command killed by a signal is likewise a report. Only a timeout is a failure
/// of the tool itself, and even then the output collected first is kept.
fn render(output: ExecOutput, timeout: Duration) -> CallToolResult {
    let exit = output.exit;

    let text = match exit {
        Exit::Code(0) if output.output.is_empty() => "(no output)".to_string(),
        Exit::Code(0) => output.output,
        Exit::TimedOut => format!(
            "command timed out after {}ms (exit code {TIMEOUT_EXIT_CODE})\n{}",
            timeout.as_millis(),
            output.output
        ),
        Exit::Code(code) => with_status(output.output, format!("exit code {code}")),
        Exit::Signal(signal) => with_status(
            output.output,
            format!(
                "exit code {} (killed by {})",
                SIGNAL_EXIT_CODE_BASE + signal,
                signal_name(signal)
            ),
        ),
    };

    let content = vec![ContentBlock::text(text)];

    if exit == Exit::TimedOut {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    }
}

fn with_status(output: String, status: String) -> String {
    if output.is_empty() {
        format!("[{status}]")
    } else if output.ends_with('\n') {
        format!("{output}[{status}]")
    } else {
        format!("{output}\n[{status}]")
    }
}

fn signal_name(signal: i32) -> Cow<'static, str> {
    let name = match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGXCPU => "SIGXCPU",
        libc::SIGXFSZ => "SIGXFSZ",
        _ => return Cow::Owned(format!("signal {signal}")),
    };
    Cow::Borrowed(name)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::exec::ExecError;
    use crate::tool::source::parse;

    struct Fake {
        seen: Mutex<Option<ExecSpec>>,
        exit: Exit,
        output: String,
        spawn_fails: bool,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                seen: Mutex::new(None),
                exit: Exit::Code(0),
                output: String::new(),
                spawn_fails: false,
            }
        }

        fn returning(exit: Exit, output: &str) -> Self {
            Self {
                exit,
                output: output.to_string(),
                ..Self::new()
            }
        }

        fn spec(&self) -> ExecSpec {
            self.seen.lock().unwrap().clone().expect("nothing ran")
        }
    }

    #[async_trait]
    impl Executor for Fake {
        async fn run(&self, spec: ExecSpec) -> Result<ExecOutput, ExecError> {
            *self.seen.lock().unwrap() = Some(spec.clone());
            if self.spawn_fails {
                return Err(ExecError::Spawn {
                    program: spec.argv.first().cloned().unwrap_or_default(),
                    source: std::io::Error::from(std::io::ErrorKind::NotFound),
                });
            }
            Ok(ExecOutput {
                output: self.output.clone(),
                truncated: false,
                exit: self.exit,
            })
        }

        async fn shutdown(&self) {}
    }

    const ECHO: &str = r#"{
        "name": "echo",
        "inputSchema": {
            "type": "object",
            "properties": { "message": { "type": "string" } },
            "required": ["message"]
        },
        "_meta": { "dev.subs/exec": { "argv": ["echo", "{message}"] } }
    }"#;

    fn tool(json: &str, executor: Arc<dyn Executor>) -> ExecTool {
        ExecTool::new(parse(json).unwrap(), executor, PathBuf::from("/daemon"))
    }

    fn args(value: serde_json::Value) -> Option<JsonObject> {
        Some(value.as_object().unwrap().clone())
    }

    fn text(result: &CallToolResult) -> String {
        match &result.content[0] {
            ContentBlock::Text(block) => block.text.clone(),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn arguments_are_substituted_into_argv() {
        let fake = Arc::new(Fake::new());
        let subject = tool(ECHO, fake.clone());

        subject
            .call(args(json!({ "message": "hello world" })))
            .await
            .unwrap();

        assert_eq!(fake.spec().argv, ["echo", "hello world"]);
    }

    /// Codex's `shell` takes its whole argv as one array parameter, so an empty
    /// array would otherwise reach the executor with nothing to run.
    #[tokio::test]
    async fn an_empty_array_cannot_leave_nothing_to_run() {
        let json = r#"{
            "name": "shell",
            "inputSchema": {
                "type": "object",
                "properties": { "command": { "type": "array", "items": { "type": "string" } } },
                "required": ["command"]
            },
            "_meta": { "dev.subs/exec": { "argv": ["{command}"] } }
        }"#;
        let fake = Arc::new(Fake::new());
        let error = tool(json, fake.clone())
            .call(args(json!({ "command": [] })))
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::EmptyArgv));
        assert_eq!(error.into_result().is_error, Some(true));
        assert!(fake.seen.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn a_non_zero_exit_is_a_successful_call() {
        let fake = Arc::new(Fake::returning(Exit::Code(1), "2 tests failed\n"));
        let result = tool(ECHO, fake)
            .call(args(json!({ "message": "x" })))
            .await
            .unwrap();

        assert_ne!(result.is_error, Some(true));
        assert_eq!(text(&result), "2 tests failed\n[exit code 1]");
    }

    #[tokio::test]
    async fn a_signal_is_reported_as_128_plus_the_signal() {
        let fake = Arc::new(Fake::returning(Exit::Signal(libc::SIGKILL), ""));
        let result = tool(ECHO, fake)
            .call(args(json!({ "message": "x" })))
            .await
            .unwrap();

        assert_ne!(result.is_error, Some(true));
        assert_eq!(text(&result), "[exit code 137 (killed by SIGKILL)]");
    }

    #[tokio::test]
    async fn a_timeout_is_an_error_that_still_reports_its_output() {
        let fake = Arc::new(Fake::returning(Exit::TimedOut, "step 1 done\n"));
        let result = tool(ECHO, fake)
            .call(args(json!({ "message": "x" })))
            .await
            .unwrap();

        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains("timed out after 30000ms"));
        assert!(text(&result).contains("step 1 done"));
    }

    #[tokio::test]
    async fn a_spawn_failure_is_an_error() {
        let fake = Arc::new(Fake {
            spawn_fails: true,
            ..Fake::new()
        });
        let error = tool(ECHO, fake)
            .call(args(json!({ "message": "x" })))
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::Exec(_)));
        assert_eq!(error.into_result().is_error, Some(true));
    }

    #[tokio::test]
    async fn a_schema_violation_is_an_error() {
        let fake = Arc::new(Fake::new());
        let error = tool(ECHO, fake.clone())
            .call(args(json!({ "message": 7 })))
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::Invalid(_)));
        assert_eq!(error.into_result().is_error, Some(true));
        assert!(fake.seen.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn a_successful_command_that_printed_nothing_says_so() {
        let fake = Arc::new(Fake::new());
        let result = tool(ECHO, fake)
            .call(args(json!({ "message": "x" })))
            .await
            .unwrap();

        assert_eq!(text(&result), "(no output)");
    }

    #[tokio::test]
    async fn a_tool_without_exec_cwd_runs_in_the_daemon_directory() {
        let fake = Arc::new(Fake::new());
        tool(ECHO, fake.clone())
            .call(args(json!({ "message": "x" })))
            .await
            .unwrap();

        assert_eq!(fake.spec().cwd, PathBuf::from("/daemon"));
    }

    #[tokio::test]
    async fn a_relative_exec_cwd_resolves_against_the_daemon_directory() {
        let json = r#"{
            "name": "t",
            "inputSchema": { "type": "object" },
            "_meta": { "dev.subs/exec": { "argv": ["ls"], "cwd": "sub/dir" } }
        }"#;
        let fake = Arc::new(Fake::new());
        tool(json, fake.clone()).call(None).await.unwrap();

        assert_eq!(fake.spec().cwd, PathBuf::from("/daemon/sub/dir"));
    }

    #[tokio::test]
    async fn an_absolute_exec_cwd_overrides_the_daemon_directory() {
        let json = r#"{
            "name": "t",
            "inputSchema": { "type": "object" },
            "_meta": { "dev.subs/exec": { "argv": ["ls"], "cwd": "/w" } }
        }"#;
        let fake = Arc::new(Fake::new());
        tool(json, fake.clone()).call(None).await.unwrap();

        assert_eq!(fake.spec().cwd, PathBuf::from("/w"));
    }

    #[tokio::test]
    async fn a_defaulted_parameter_is_applied_before_substitution() {
        let json = r#"{
            "name": "t",
            "inputSchema": {
                "type": "object",
                "properties": { "dir": { "type": "string", "default": "." } }
            },
            "_meta": { "dev.subs/exec": { "argv": ["ls", "{dir}"] } }
        }"#;
        let fake = Arc::new(Fake::new());
        tool(json, fake.clone()).call(None).await.unwrap();

        assert_eq!(fake.spec().argv, ["ls", "."]);
    }

    #[tokio::test]
    async fn stdin_is_substituted_and_passed_through() {
        let json = r#"{
            "name": "t",
            "inputSchema": {
                "type": "object",
                "properties": { "body": { "type": "string" } },
                "required": ["body"]
            },
            "_meta": { "dev.subs/exec": { "argv": ["tee"], "stdin": "{body}" } }
        }"#;
        let fake = Arc::new(Fake::new());
        tool(json, fake.clone())
            .call(args(json!({ "body": "payload" })))
            .await
            .unwrap();

        assert_eq!(fake.spec().stdin.as_deref(), Some("payload"));
    }

    #[tokio::test]
    async fn timeout_and_truncation_reach_the_executor() {
        let json = r#"{
            "name": "t",
            "inputSchema": { "type": "object" },
            "_meta": {
                "dev.subs/exec": {
                    "argv": ["true"],
                    "timeoutMs": 1500,
                    "maxOutputBytes": 64
                }
            }
        }"#;
        let fake = Arc::new(Fake::new());
        tool(json, fake.clone()).call(None).await.unwrap();

        assert_eq!(fake.spec().timeout, Duration::from_millis(1500));
        assert_eq!(fake.spec().max_output_bytes, 64);
    }
}
