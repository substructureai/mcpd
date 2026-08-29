pub mod exec_tool;
pub mod lock;
pub mod registry;
pub mod schema;
pub mod source;
pub mod substitute;

use std::sync::Arc;

use async_trait::async_trait;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool};
use serde::Deserialize;

use crate::exec::ExecError;
use crate::tool::schema::InputSchema;

pub const EXEC_META_KEY: &str = "dev.subs/exec";

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 50_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecMeta {
    pub argv: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub stdin: Option<String>,
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
    #[serde(default)]
    pub lock: Vec<String>,
}

impl ExecMeta {
    pub fn templates(&self) -> impl Iterator<Item = &str> {
        self.argv
            .iter()
            .map(String::as_str)
            .chain(self.cwd.as_deref())
            .chain(self.stdin.as_deref())
            .chain(self.lock.iter().map(String::as_str))
    }
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn default_max_output_bytes() -> usize {
    DEFAULT_MAX_OUTPUT_BYTES
}

pub struct ToolDef {
    pub tool: Tool,
    pub exec: ExecMeta,
    pub schema: InputSchema,
}

impl ToolDef {
    pub fn name(&self) -> &str {
        &self.tool.name
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("tool definition is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("tool `{0}` has no `_meta.{EXEC_META_KEY}`")]
    MissingExec(String),
    #[error("tool `{name}` has a malformed `_meta.{EXEC_META_KEY}`: {source}")]
    MalformedExec {
        name: String,
        source: serde_json::Error,
    },
    #[error("tool `{0}` has an empty argv")]
    EmptyArgv(String),
    #[error("tool name must not be empty")]
    EmptyName,
    #[error("tool `{0}` is defined more than once")]
    Duplicate(String),
    #[error("tool `{name}` has an unusable inputSchema: {reason}")]
    Schema { name: String, reason: String },
    #[error(
        "tool `{name}` substitutes `{{{param}}}`, but `{param}` is neither required nor defaulted"
    )]
    OptionalParam { name: String, param: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("invalid arguments: {0}")]
    Invalid(String),
    #[error("missing parameter `{0}`")]
    MissingParam(String),
    #[error("parameter `{param}` cannot be substituted: {reason}")]
    BadValue { param: String, reason: String },
    #[error("the arguments left no command to run")]
    EmptyArgv,
    #[error(transparent)]
    Exec(#[from] ExecError),
}

impl ToolError {
    /// The one place a tool failure becomes `isError: true`.
    pub fn into_result(self) -> CallToolResult {
        CallToolResult::error(vec![ContentBlock::text(self.to_string())])
    }
}

/// One tool. Every variant of `ToolError` is the tool failing to run, which is
/// what `isError` means — a command exiting non-zero is not one of them.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    fn descriptor(&self) -> &Tool;
    async fn call(&self, arguments: Option<JsonObject>) -> Result<CallToolResult, ToolError>;
}

pub trait ToolRegistry: Send + Sync {
    fn list(&self) -> Vec<Tool>;
    fn get(&self, name: &str) -> Option<Arc<dyn ToolHandler>>;
}
