use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use rmcp::model::Implementation;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use mcpd::VERSION;
use mcpd::cli::{Cli, credentials, mcp_path, working_dir};
use mcpd::exec::Executor;
use mcpd::exec::process::ProcessExecutor;
use mcpd::tool::exec_tool::ExecTool;
use mcpd::tool::registry::StaticRegistry;
use mcpd::tool::{ToolHandler, ToolRegistry, source};
use mcpd::transport::auth::{Anonymous, Authenticator, BearerToken};
use mcpd::transport::handler::McpdHandler;
use mcpd::transport::server;

#[tokio::main]
async fn main() -> Result<()> {
    let settings = Cli::parse().load()?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mcpd=info,tower_http=info".into()),
        )
        .init();

    let auth: Arc<dyn Authenticator> = match credentials(settings.no_auth)? {
        Some(token) => Arc::new(BearerToken::new(&token)),
        None => {
            tracing::warn!(
                "serving without authentication: anyone who can reach {} can run these tools",
                settings.bind
            );
            Arc::new(Anonymous)
        }
    };
    let cwd = working_dir(settings.cwd)?;
    let path = mcp_path(settings.mcp_path)?;
    let executor: Arc<dyn Executor> = Arc::new(ProcessExecutor::new());
    let handlers = source::load(&settings.tools)?
        .into_iter()
        .map(|def| {
            Arc::new(ExecTool::new(def, executor.clone(), cwd.clone())) as Arc<dyn ToolHandler>
        })
        .collect();

    let registry = Arc::new(StaticRegistry::new(handlers)?);
    tracing::info!(tools = registry.list().len(), "registered");

    let handler = McpdHandler {
        registry,
        server_info: Implementation::new(settings.name, VERSION),
        instructions: settings.instructions,
        list_ttl_ms: settings.list_ttl_ms,
    };

    let shutdown = CancellationToken::new();

    let router = server::router(auth, handler, shutdown.clone(), &path);
    let listener = TcpListener::bind(&settings.bind).await?;
    tracing::info!(
        endpoint = %format!("http://{}{path}", settings.bind),
        health = %format!("http://{}{}", settings.bind, server::HEALTH_PATH),
        cwd = %cwd.display(),
        version = VERSION,
        "listening"
    );

    let served = axum::serve(listener, router)
        .with_graceful_shutdown(server::shutdown_signal(shutdown))
        .await;

    // Unconditionally, and before propagating any serve error. Not only so a
    // deploy mid-command leaves no orphans: cancelling an in-flight call drops
    // its future, but the blocking task reading that command's output cannot be
    // cancelled, and it stays blocked in `read()` while the process group holds
    // the pipe open. Tokio waits for the blocking pool on the way out, so
    // without this the daemon never exits at all.
    executor.shutdown().await;

    served?;
    Ok(())
}
