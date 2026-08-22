use anyhow::Result;
use rmcp::ServiceExt;
use rmcp::service::{QuitReason, ServerInitializeError};
use rmcp::transport::stdio;
use tokio_util::sync::CancellationToken;

use crate::transport::handler::McpdHandler;

pub async fn serve(handler: McpdHandler, shutdown: CancellationToken) -> Result<()> {
    let service = match handler.serve_with_ct(stdio(), shutdown).await {
        Ok(service) => service,
        Err(ServerInitializeError::Cancelled) => return Ok(()),
        Err(e) => return Err(e.into()),
    };

    match service.waiting().await? {
        QuitReason::Cancelled => tracing::info!("stdio session cancelled"),
        QuitReason::Closed => tracing::info!("stdio session closed"),
        reason => tracing::warn!(?reason, "stdio session ended"),
    }

    Ok(())
}
