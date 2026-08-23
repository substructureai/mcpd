use std::borrow::Cow;
use std::sync::Arc;

use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, Implementation, InitializeResult,
    ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};

use crate::tool::{ToolError, ToolRegistry};

const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V_2026_07_28;

static PROTOCOL_VERSIONS: &[ProtocolVersion] = &[PROTOCOL_VERSION];

#[derive(Clone)]
pub struct McpdHandler {
    pub registry: Arc<dyn ToolRegistry>,
    pub server_info: Implementation,
    pub instructions: Option<String>,
    pub list_ttl_ms: u64,
}

impl ServerHandler for McpdHandler {
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(PROTOCOL_VERSIONS)
    }

    fn get_info(&self) -> ServerInfo {
        let capabilities = ServerCapabilities::builder().enable_tools().build();
        let mut info = InitializeResult::new(capabilities);
        info.protocol_version = PROTOCOL_VERSION;
        info.server_info = self.server_info.clone();
        info.instructions = self.instructions.clone();
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.registry.list())
            .with_ttl_ms(self.list_ttl_ms)
            .with_cache_scope(CacheScope::Public))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tool = self.registry.get(&request.name).ok_or_else(|| {
            ErrorData::invalid_params(format!("unknown tool: {}", request.name), None)
        })?;

        let result = tool
            .call(request.arguments)
            .await
            .unwrap_or_else(ToolError::into_result);

        Ok(result.into())
    }
}
