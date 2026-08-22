use serde_json::{Value, json};

pub const TOKEN: &str = "integration-token";
pub const PROTOCOL: &str = "2026-07-28";

pub fn sh(timeout_ms: u64, max_output_bytes: usize) -> String {
    json!({
        "name": "sh",
        "description": "Run a shell command.",
        "inputSchema": {
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"],
        },
        "_meta": {
            "dev.subs/exec": {
                "argv": ["/bin/sh", "-lc", "{command}"],
                "timeoutMs": timeout_ms,
                "maxOutputBytes": max_output_bytes,
            },
        },
    })
    .to_string()
}

pub fn text(response: &Value) -> String {
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text content in {response}"))
        .to_string()
}

pub fn is_error(response: &Value) -> bool {
    response["result"]["isError"].as_bool().unwrap_or(false)
}
