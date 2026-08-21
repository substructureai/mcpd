pub mod collect;
pub mod process;

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;

/// Reported for a command killed by its timeout, matching `timeout(1)` and
/// Codex's `EXEC_TIMEOUT_EXIT_CODE`. Preferred over the 137 the SIGKILL would
/// otherwise produce, since the timeout is the more specific cause.
pub const TIMEOUT_EXIT_CODE: i32 = 124;

/// Shell convention: a command killed by signal N reports 128 + N.
pub const SIGNAL_EXIT_CODE_BASE: i32 = 128;

/// Variables a model-reachable command must never inherit. The daemon's own
/// bearer token is in its environment, and `bash` can read it.
pub const NON_INHERITABLE_ENV_VARS: &[&str] = &[crate::cli::TOKEN_ENV];

/// Bound on draining output after the command is gone. A grandchild that
/// escaped the process group can hold the pipe open forever; without this the
/// request never returns.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct ExecSpec {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub stdin: Option<String>,
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

/// How the command ended. A timeout belongs here rather than in [`ExecError`]
/// because the command did run — discarding what it printed first is the one
/// thing a model debugging a hang most needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    Signal(i32),
    TimedOut,
}

#[derive(Debug, Clone)]
pub struct ExecOutput {
    pub output: String,
    pub truncated: bool,
    pub exit: Exit,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("no command to run")]
    EmptyArgv,
    #[error("working directory {0} is not usable")]
    NoWorkingDir(PathBuf),
    #[error("cannot run `{program}`: {source}")]
    Spawn {
        program: String,
        source: std::io::Error,
    },
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

#[async_trait]
pub trait Executor: Send + Sync {
    async fn run(&self, spec: ExecSpec) -> Result<ExecOutput, ExecError>;

    /// Kill every process group still running. A supervisor stopping the
    /// daemon mid-command would otherwise leave exactly the orphans the
    /// timeout exists to prevent.
    async fn shutdown(&self);
}
