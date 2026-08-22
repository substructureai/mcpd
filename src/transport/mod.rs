pub mod auth;
pub mod handler;
pub mod server;
pub mod stdio;

use tokio_util::sync::CancellationToken;

/// Resolves on SIGINT or SIGTERM, then cancels the token. Shared by both
/// transports: what a signal ends is the daemon, not any one way of serving it.
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
