use std::collections::BTreeSet;

use rmcp::model::JsonObject;
use serde_json::Value;

use crate::tool::ToolError;

pub fn argv(
    template: &[String],
    arguments: &JsonObject,
    params: &BTreeSet<String>,
) -> Result<Vec<String>, ToolError> {
    let mut out = Vec::with_capacity(template.len());

    for element in template {
        let found = placeholders(element, params);

        match found.as_slice() {
            [] => out.push(element.clone()),
            [(start, end, name)] if *start == 0 && *end == element.len() => {
                match lookup(arguments, name)? {
                    Value::Array(items) => {
                        for item in items {
                            out.push(scalar(name, item)?);
                        }
                    }
                    value => out.push(scalar(name, value)?),
                }
            }
            _ => out.push(splice(element, &found, arguments)?),
        }
    }

    Ok(out)
}

pub fn text(
    template: &str,
    arguments: &JsonObject,
    params: &BTreeSet<String>,
) -> Result<String, ToolError> {
    let found = placeholders(template, params);
    splice(template, &found, arguments)
}

pub fn referenced(template: &str, params: &BTreeSet<String>) -> Vec<String> {
    placeholders(template, params)
        .into_iter()
        .map(|(_, _, name)| name)
        .collect()
}

fn splice(
    template: &str,
    found: &[(usize, usize, String)],
    arguments: &JsonObject,
) -> Result<String, ToolError> {
    let mut out = String::with_capacity(template.len());
    let mut cursor = 0;

    for (start, end, name) in found {
        out.push_str(&template[cursor..*start]);
        let value = lookup(arguments, name)?;
        if value.is_array() {
            return Err(ToolError::BadValue {
                param: name.clone(),
                reason: "an array can only replace an argument on its own".to_string(),
            });
        }
        out.push_str(&scalar(name, value)?);
        cursor = *end;
    }

    out.push_str(&template[cursor..]);
    Ok(out)
}

fn lookup<'a>(arguments: &'a JsonObject, name: &str) -> Result<&'a Value, ToolError> {
    arguments
        .get(name)
        .ok_or_else(|| ToolError::MissingParam(name.to_string()))
}

fn scalar(name: &str, value: &Value) -> Result<String, ToolError> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        other => Err(ToolError::BadValue {
            param: name.to_string(),
            reason: format!("{} is not a string, number or boolean", kind(other)),
        }),
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Only names the schema declares are placeholders. Everything else is
/// literal, so `awk '{print}'` survives a trip through substitution.
fn placeholders(template: &str, params: &BTreeSet<String>) -> Vec<(usize, usize, String)> {
    let bytes = template.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }

        let mut j = i + 1;
        while j < bytes.len()
            && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'-')
        {
            j += 1;
        }

        if j > i + 1 && j < bytes.len() && bytes[j] == b'}' {
            let name = &template[i + 1..j];
            if params.contains(name) {
                found.push((i, j + 1, name.to_string()));
                i = j + 1;
                continue;
            }
        }

        i += 1;
    }

    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn params(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn args(value: Value) -> JsonObject {
        value.as_object().unwrap().clone()
    }

    fn template(elements: &[&str]) -> Vec<String> {
        elements.iter().map(|e| e.to_string()).collect()
    }

    #[test]
    fn a_string_replaces_the_element_it_stands_in_for() {
        let out = argv(
            &template(&["/bin/bash", "-lc", "{command}"]),
            &args(json!({ "command": "echo hi" })),
            &params(&["command"]),
        )
        .unwrap();
        assert_eq!(out, ["/bin/bash", "-lc", "echo hi"]);
    }

    #[test]
    fn a_value_with_spaces_stays_one_argument() {
        let out = argv(
            &template(&["echo", "{msg}"]),
            &args(json!({ "msg": "a b c" })),
            &params(&["msg"]),
        )
        .unwrap();
        assert_eq!(out, ["echo", "a b c"]);
    }

    #[test]
    fn numbers_and_booleans_are_stringified() {
        let out = argv(
            &template(&["run", "{n}", "{flag}"]),
            &args(json!({ "n": 42, "flag": true })),
            &params(&["n", "flag"]),
        )
        .unwrap();
        assert_eq!(out, ["run", "42", "true"]);
    }

    #[test]
    fn an_array_splices_into_several_arguments() {
        let out = argv(
            &template(&["rg", "{pattern}", "{paths}"]),
            &args(json!({ "pattern": "fn", "paths": ["src", "tests"] })),
            &params(&["pattern", "paths"]),
        )
        .unwrap();
        assert_eq!(out, ["rg", "fn", "src", "tests"]);
    }

    #[test]
    fn an_empty_array_splices_nothing() {
        let out = argv(
            &template(&["rg", "{paths}"]),
            &args(json!({ "paths": [] })),
            &params(&["paths"]),
        )
        .unwrap();
        assert_eq!(out, ["rg"]);
    }

    #[test]
    fn an_array_cannot_splice_into_a_larger_string() {
        let err = argv(
            &template(&["--paths={paths}"]),
            &args(json!({ "paths": ["a", "b"] })),
            &params(&["paths"]),
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::BadValue { .. }));
    }

    #[test]
    fn an_object_is_rejected() {
        let err = argv(
            &template(&["{opts}"]),
            &args(json!({ "opts": { "a": 1 } })),
            &params(&["opts"]),
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::BadValue { .. }));
    }

    #[test]
    fn null_is_rejected() {
        let err = argv(
            &template(&["{x}"]),
            &args(json!({ "x": null })),
            &params(&["x"]),
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::BadValue { .. }));
    }

    #[test]
    fn a_declared_parameter_with_no_value_is_an_error() {
        let err = argv(
            &template(&["{command}"]),
            &args(json!({})),
            &params(&["command"]),
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::MissingParam(p) if p == "command"));
    }

    #[test]
    fn a_placeholder_can_sit_inside_a_larger_argument() {
        let out = argv(
            &template(&["--file={path}"]),
            &args(json!({ "path": "a.txt" })),
            &params(&["path"]),
        )
        .unwrap();
        assert_eq!(out, ["--file=a.txt"]);
    }

    #[test]
    fn several_placeholders_can_share_one_argument() {
        let out = argv(
            &template(&["{a}:{b}"]),
            &args(json!({ "a": "x", "b": "y" })),
            &params(&["a", "b"]),
        )
        .unwrap();
        assert_eq!(out, ["x:y"]);
    }

    #[test]
    fn braces_the_schema_does_not_declare_are_literal() {
        let out = argv(
            &template(&["awk", "{print $1}", "{nope}", "{}"]),
            &args(json!({})),
            &params(&["command"]),
        )
        .unwrap();
        assert_eq!(out, ["awk", "{print $1}", "{nope}", "{}"]);
    }

    /// Claude Code names parameters `-i`, `-n` and `-C`, so a hyphen has to be
    /// part of a placeholder name for those schemas to be expressible.
    #[test]
    fn a_parameter_name_may_contain_a_hyphen() {
        let out = argv(
            &template(&["rg", "--context", "{-C}"]),
            &args(json!({ "-C": 3 })),
            &params(&["-C"]),
        )
        .unwrap();
        assert_eq!(out, ["rg", "--context", "3"]);
    }

    #[test]
    fn a_substituted_value_is_never_rescanned() {
        let out = argv(
            &template(&["{a}"]),
            &args(json!({ "a": "{b}", "b": "surprise" })),
            &params(&["a", "b"]),
        )
        .unwrap();
        assert_eq!(out, ["{b}"]);
    }

    #[test]
    fn text_substitutes_without_splicing() {
        let out = text("{cwd}", &args(json!({ "cwd": "/w" })), &params(&["cwd"])).unwrap();
        assert_eq!(out, "/w");
    }

    #[test]
    fn text_carries_a_whole_payload() {
        let out = text(
            "{content}",
            &args(json!({ "content": "line one\nline two\n" })),
            &params(&["content"]),
        )
        .unwrap();
        assert_eq!(out, "line one\nline two\n");
    }
}
