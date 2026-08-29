use rmcp::model::Tool;

use crate::tool::schema::InputSchema;
use crate::tool::{EXEC_META_KEY, ExecMeta, LoadError, ToolDef, substitute};

pub fn load(definitions: &[String]) -> Result<Vec<ToolDef>, LoadError> {
    definitions.iter().map(|json| parse(json)).collect()
}

pub fn parse(json: &str) -> Result<ToolDef, LoadError> {
    let mut tool: Tool = serde_json::from_str(json)?;

    if tool.name.is_empty() {
        return Err(LoadError::EmptyName);
    }
    let name = tool.name.to_string();

    let raw = tool
        .meta
        .as_ref()
        .and_then(|meta| meta.0.get(EXEC_META_KEY))
        .ok_or_else(|| LoadError::MissingExec(name.clone()))?;

    let exec: ExecMeta =
        serde_json::from_value(raw.clone()).map_err(|source| LoadError::MalformedExec {
            name: name.clone(),
            source,
        })?;

    if exec.argv.is_empty() {
        return Err(LoadError::EmptyArgv(name));
    }

    let schema = InputSchema::compile(&name, &tool.input_schema)?;

    for template in exec.templates() {
        for param in substitute::referenced(template, schema.params()) {
            if !schema.always_supplied(&param) {
                return Err(LoadError::OptionalParam { name, param });
            }
        }
    }

    tool.meta = None;

    Ok(ToolDef { tool, exec, schema })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASH: &str = r#"{
        "name": "bash",
        "description": "Run a shell command.",
        "inputSchema": {
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"]
        },
        "annotations": { "destructiveHint": true },
        "_meta": {
            "dev.subs/exec": {
                "argv": ["/bin/bash", "-lc", "{command}"],
                "cwd": "{cwd}",
                "timeoutMs": 5000,
                "stdin": null
            }
        }
    }"#;

    #[test]
    fn a_definition_keeps_its_tool_and_its_exec() {
        let def = parse(BASH).unwrap();
        assert_eq!(def.name(), "bash");
        assert_eq!(
            def.tool.description.as_deref(),
            Some("Run a shell command.")
        );
        assert_eq!(def.exec.argv, ["/bin/bash", "-lc", "{command}"]);
        assert_eq!(def.exec.cwd.as_deref(), Some("{cwd}"));
        assert_eq!(def.exec.timeout_ms, 5000);
        assert!(def.exec.stdin.is_none());
    }

    #[test]
    fn exec_details_never_reach_the_served_tool() {
        let def = parse(BASH).unwrap();
        assert!(def.tool.meta.is_none());
    }

    #[test]
    fn annotations_and_schema_survive_untouched() {
        let def = parse(BASH).unwrap();
        assert_eq!(def.tool.annotations.unwrap().destructive_hint, Some(true));
        assert!(def.tool.input_schema.contains_key("properties"));
    }

    #[test]
    fn timeout_and_truncation_have_defaults() {
        let json = r#"{
            "name": "t",
            "inputSchema": { "type": "object" },
            "_meta": { "dev.subs/exec": { "argv": ["true"] } }
        }"#;
        let def = parse(json).unwrap();
        assert_eq!(def.exec.timeout_ms, 30_000);
        assert_eq!(def.exec.max_output_bytes, 50_000);
    }

    #[test]
    fn a_tool_without_exec_meta_is_rejected() {
        let json = r#"{ "name": "t", "inputSchema": { "type": "object" } }"#;
        assert!(matches!(parse(json), Err(LoadError::MissingExec(n)) if n == "t"));
    }

    #[test]
    fn an_empty_argv_is_rejected() {
        let json = r#"{
            "name": "t",
            "inputSchema": { "type": "object" },
            "_meta": { "dev.subs/exec": { "argv": [] } }
        }"#;
        assert!(matches!(parse(json), Err(LoadError::EmptyArgv(n)) if n == "t"));
    }

    #[test]
    fn a_misspelled_exec_key_is_rejected_rather_than_ignored() {
        let json = r#"{
            "name": "t",
            "inputSchema": { "type": "object" },
            "_meta": { "dev.subs/exec": { "argv": ["true"], "timeout_ms": 10 } }
        }"#;
        assert!(matches!(parse(json), Err(LoadError::MalformedExec { .. })));
    }

    #[test]
    fn malformed_json_is_rejected() {
        assert!(matches!(parse("{"), Err(LoadError::Json(_))));
    }

    #[test]
    fn a_substituted_parameter_that_could_be_absent_is_rejected() {
        let json = r#"{
            "name": "t",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string" } }
            },
            "_meta": { "dev.subs/exec": { "argv": ["cat", "{path}"] } }
        }"#;
        assert!(matches!(
            parse(json),
            Err(LoadError::OptionalParam { param, .. }) if param == "path"
        ));
    }

    #[test]
    fn a_required_parameter_may_be_substituted() {
        let json = r#"{
            "name": "t",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            },
            "_meta": { "dev.subs/exec": { "argv": ["cat", "{path}"] } }
        }"#;
        assert!(parse(json).is_ok());
    }

    #[test]
    fn a_defaulted_parameter_may_be_substituted() {
        let json = r#"{
            "name": "t",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string", "default": "." } }
            },
            "_meta": { "dev.subs/exec": { "argv": ["cat", "{path}"] } }
        }"#;
        assert!(parse(json).is_ok());
    }

    #[test]
    fn cwd_and_stdin_are_checked_too() {
        let cwd = r#"{
            "name": "t",
            "inputSchema": {
                "type": "object",
                "properties": { "dir": { "type": "string" } }
            },
            "_meta": { "dev.subs/exec": { "argv": ["ls"], "cwd": "{dir}" } }
        }"#;
        assert!(matches!(
            parse(cwd),
            Err(LoadError::OptionalParam { param, .. }) if param == "dir"
        ));

        let stdin = r#"{
            "name": "t",
            "inputSchema": {
                "type": "object",
                "properties": { "content": { "type": "string" } }
            },
            "_meta": { "dev.subs/exec": { "argv": ["tee"], "stdin": "{content}" } }
        }"#;
        assert!(matches!(
            parse(stdin),
            Err(LoadError::OptionalParam { param, .. }) if param == "content"
        ));

        let lock = r#"{
            "name": "t",
            "inputSchema": {
                "type": "object",
                "properties": { "file_path": { "type": "string" } }
            },
            "_meta": {
                "dev.subs/exec": { "argv": ["edit"], "lock": ["{file_path}"] }
            }
        }"#;
        assert!(matches!(
            parse(lock),
            Err(LoadError::OptionalParam { param, .. }) if param == "file_path"
        ));
    }

    #[test]
    fn a_definition_without_a_lock_holds_no_keys() {
        assert!(parse(BASH).unwrap().exec.lock.is_empty());
    }

    #[test]
    fn an_undeclared_brace_is_not_a_parameter_to_check() {
        let json = r#"{
            "name": "t",
            "inputSchema": { "type": "object" },
            "_meta": { "dev.subs/exec": { "argv": ["awk", "{print $1}", "{nope}"] } }
        }"#;
        assert!(parse(json).is_ok());
    }
}
