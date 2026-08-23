#![deny(clippy::print_stdout, clippy::print_stderr)]

use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use rmcp::model::Implementation;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use mcpd::VERSION;
use mcpd::cli::{
    Cli, HttpEndpoint, Settings, TOKEN_ENV, Transport, credentials, mcp_path, working_dir,
};
use mcpd::exec::Executor;
use mcpd::exec::process::ProcessExecutor;
use mcpd::tool::exec_tool::ExecTool;
use mcpd::tool::registry::StaticRegistry;
use mcpd::tool::{ToolHandler, source};
use mcpd::transport::auth::{Anonymous, Authenticator, BearerToken};
use mcpd::transport::handler::{HTTP_VERSIONS, McpdHandler, STDIO_VERSIONS};
use mcpd::transport::{server, shutdown_signal, stdio};

/// The runtime is built here rather than by `#[tokio::main]` so it can be left
/// behind instead of dropped. Dropping it waits for the blocking pool, and
/// `tokio::io::stdin` parks a read there that no signal can cancel.
fn main() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let served = runtime.block_on(serve());
    runtime.shutdown_background();

    served
}

async fn serve() -> Result<()> {
    let settings = Cli::parse().load()?;

    logging();

    let transport = settings.transport();
    let Settings {
        name,
        title,
        cwd,
        tools,
        instructions,
        list_ttl_ms,
        ..
    } = settings;

    let cwd = working_dir(cwd)?;
    let executor: Arc<dyn Executor> = Arc::new(ProcessExecutor::scrubbing([TOKEN_ENV.to_string()]));

    let defs = source::load(&tools)?;
    let count = defs.len();
    let handlers = defs
        .into_iter()
        .map(|def| {
            Arc::new(ExecTool::new(def, executor.clone(), cwd.clone())) as Arc<dyn ToolHandler>
        })
        .collect();

    let registry = Arc::new(StaticRegistry::new(handlers)?);
    tracing::info!(tools = count, "registered");

    let server_info = match title {
        Some(title) => Implementation::new(name, VERSION).with_title(title),
        None => Implementation::new(name, VERSION),
    };

    let handler = McpdHandler {
        registry,
        server_info,
        instructions,
        list_ttl_ms,
        protocol_versions: match transport {
            Transport::Stdio => STDIO_VERSIONS,
            Transport::Http(_) => HTTP_VERSIONS,
        },
    };

    let shutdown = CancellationToken::new();

    let served = match transport {
        Transport::Stdio => over_stdio(handler, shutdown, &cwd).await,
        Transport::Http(endpoint) => over_http(endpoint, handler, shutdown, &cwd).await,
    };

    // Unconditionally, and before propagating any serve error, so that a deploy
    // mid-command leaves no orphans: the commands are in their own process
    // groups and nothing else is going to reap them.
    executor.shutdown().await;

    served
}

/// Always stderr: stdout is the protocol under `--stdio`, and logs on stderr is
/// what every other MCP server does with it.
fn logging() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mcpd=info,tower_http=info".into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
}

async fn over_stdio(handler: McpdHandler, shutdown: CancellationToken, cwd: &Path) -> Result<()> {
    tracing::info!(cwd = %cwd.display(), version = VERSION, "serving on stdin and stdout");

    tokio::spawn(shutdown_signal(shutdown.clone()));

    stdio::serve(handler, shutdown).await
}

async fn over_http(
    endpoint: HttpEndpoint,
    handler: McpdHandler,
    shutdown: CancellationToken,
    cwd: &Path,
) -> Result<()> {
    let HttpEndpoint {
        bind,
        mcp_path: path,
        no_auth,
    } = endpoint;

    let auth: Arc<dyn Authenticator> = match credentials(no_auth)? {
        Some(token) => Arc::new(BearerToken::new(&token)),
        None => {
            tracing::warn!(
                "serving without authentication: anyone who can reach {bind} can run these tools"
            );
            Arc::new(Anonymous)
        }
    };
    let path = mcp_path(path)?;

    let router = server::router(auth, handler, shutdown.clone(), &path);
    let listener = TcpListener::bind(&bind).await?;
    let bound = listener.local_addr()?;
    tracing::info!(
        endpoint = %format!("http://{bound}{path}"),
        health = %format!("http://{bound}{}", server::HEALTH_PATH),
        cwd = %cwd.display(),
        version = VERSION,
        "listening"
    );

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal(shutdown))
        .await?;

    Ok(())
}
