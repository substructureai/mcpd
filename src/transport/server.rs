use std::sync::Arc;

use axum::{
    Router,
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::never::NeverSessionManager,
};
use tokio_util::sync::CancellationToken;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};

use crate::VERSION;
use crate::transport::auth::Authenticator;
use crate::transport::handler::McpdHandler;

pub const HEALTH_PATH: &str = "/health";
pub const DEFAULT_MCP_PATH: &str = "/mcp";

pub fn router(
    auth: Arc<dyn Authenticator>,
    handler: McpdHandler,
    shutdown: CancellationToken,
    mcp_path: &str,
) -> Router {
    let mcp = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(NeverSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_json_response(true)
            .with_legacy_session_mode(false)
            .with_stateless_protocol_metadata_required(true)
            .disable_allowed_hosts()
            .with_cancellation_token(shutdown),
    );

    let protected = Router::new()
        .route_service(mcp_path, mcp)
        .layer(middleware::from_fn_with_state(auth, require_bearer));

    Router::new()
        .route(HEALTH_PATH, get(health))
        .merge(protected)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(tracing::Level::INFO))
                .on_response(DefaultOnResponse::new().level(tracing::Level::INFO)),
        )
}

async fn health() -> &'static str {
    VERSION
}

async fn require_bearer(
    State(auth): State<Arc<dyn Authenticator>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    match auth.authenticate(request.headers()) {
        Ok(()) => Ok(next.run(request).await),
        Err(_) => Err(StatusCode::UNAUTHORIZED),
    }
}

pub async fn shutdown_signal(shutdown: CancellationToken) {
    let interrupt = async {
        tokio::signal::ctrl_c().await.ok();
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => tracing::warn!(error = %e, "cannot listen for SIGTERM"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = interrupt => {}
        _ = terminate => {}
    }
    tracing::info!("shutting down");
    shutdown.cancel();
}
