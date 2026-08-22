use std::collections::HashMap;
use std::sync::Arc;

use rmcp::model::Tool;

use crate::tool::{LoadError, ToolHandler, ToolRegistry};

pub struct StaticRegistry {
    by_name: HashMap<String, Arc<dyn ToolHandler>>,
    listed: Vec<Tool>,
}

impl StaticRegistry {
    pub fn new(handlers: Vec<Arc<dyn ToolHandler>>) -> Result<Self, LoadError> {
        let mut by_name = HashMap::with_capacity(handlers.len());
        let mut listed = Vec::with_capacity(handlers.len());

        for handler in handlers {
            let tool = handler.descriptor().clone();
            let name = tool.name.to_string();

            if by_name.insert(name.clone(), handler).is_some() {
                return Err(LoadError::Duplicate(name));
            }
            listed.push(tool);
        }

        Ok(Self { by_name, listed })
    }
}

impl ToolRegistry for StaticRegistry {
    fn list(&self) -> Vec<Tool> {
        self.listed.clone()
    }

    fn get(&self, name: &str) -> Option<Arc<dyn ToolHandler>> {
        self.by_name.get(name).cloned()
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use rmcp::model::{CallToolResult, JsonObject};

    use super::*;
    use crate::tool::ToolError;
    use crate::tool::source::parse;

    struct Stub(Tool);

    #[async_trait]
    impl ToolHandler for Stub {
        fn descriptor(&self) -> &Tool {
            &self.0
        }

        async fn call(&self, _arguments: Option<JsonObject>) -> Result<CallToolResult, ToolError> {
            Ok(CallToolResult::success(Vec::new()))
        }
    }

    fn handler(name: &str) -> Arc<dyn ToolHandler> {
        let def = parse(&format!(
            r#"{{
                "name": "{name}",
                "inputSchema": {{ "type": "object" }},
                "_meta": {{ "dev.subs/exec": {{ "argv": ["true"] }} }}
            }}"#
        ))
        .unwrap();
        Arc::new(Stub(def.tool))
    }

    #[test]
    fn tools_are_listed_in_declaration_order() {
        let registry = StaticRegistry::new(vec![handler("b"), handler("a")]).unwrap();
        let names: Vec<_> = registry.list().iter().map(|t| t.name.to_string()).collect();
        assert_eq!(names, ["b", "a"]);
    }

    #[test]
    fn a_declared_tool_is_retrievable_by_name() {
        let registry = StaticRegistry::new(vec![handler("a")]).unwrap();
        assert_eq!(registry.get("a").unwrap().descriptor().name, "a");
    }

    #[test]
    fn an_undeclared_tool_is_absent() {
        let registry = StaticRegistry::new(vec![handler("a")]).unwrap();
        assert!(registry.get("nope").is_none());
    }

    #[test]
    fn a_duplicate_name_is_rejected_at_startup() {
        let result = StaticRegistry::new(vec![handler("a"), handler("a")]);
        assert!(matches!(result, Err(LoadError::Duplicate(n)) if n == "a"));
    }

    #[test]
    fn no_listed_tool_carries_exec_details() {
        let registry = StaticRegistry::new(vec![handler("a"), handler("b")]).unwrap();
        assert!(registry.list().iter().all(|t| t.meta.is_none()));
    }
}
