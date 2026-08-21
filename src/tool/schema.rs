use std::collections::BTreeSet;

use jsonschema::Validator;
use rmcp::model::JsonObject;
use serde_json::Value;

use crate::tool::{LoadError, ToolError};

pub struct InputSchema {
    validator: Validator,
    defaults: Vec<(String, Value)>,
    params: BTreeSet<String>,
    required: BTreeSet<String>,
}

impl InputSchema {
    pub fn compile(name: &str, schema: &JsonObject) -> Result<Self, LoadError> {
        let document = Value::Object(schema.clone());
        let validator = jsonschema::validator_for(&document).map_err(|e| LoadError::Schema {
            name: name.to_string(),
            reason: e.to_string(),
        })?;

        let mut defaults = Vec::new();
        let mut params = BTreeSet::new();
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (property, spec) in properties {
                params.insert(property.clone());
                if let Some(default) = spec.get("default") {
                    defaults.push((property.clone(), default.clone()));
                }
            }
        }

        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            validator,
            defaults,
            params,
            required,
        })
    }

    pub fn params(&self) -> &BTreeSet<String> {
        &self.params
    }

    pub fn always_supplied(&self, param: &str) -> bool {
        self.required.contains(param) || self.defaults.iter().any(|(name, _)| name == param)
    }

    pub fn prepare(&self, arguments: Option<JsonObject>) -> Result<JsonObject, ToolError> {
        let mut arguments = arguments.unwrap_or_default();

        let document = Value::Object(arguments.clone());
        self.validator
            .validate(&document)
            .map_err(|e| ToolError::Invalid(e.to_string()))?;

        for (property, default) in &self.defaults {
            if !arguments.contains_key(property) {
                arguments.insert(property.clone(), default.clone());
            }
        }

        Ok(arguments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema(value: Value) -> InputSchema {
        let object = value.as_object().unwrap().clone();
        InputSchema::compile("t", &object).unwrap()
    }

    fn bash() -> InputSchema {
        schema(json!({
            "type": "object",
            "properties": {
                "command": { "type": "string" },
                "cwd": { "type": "string", "default": "/w" }
            },
            "required": ["command"]
        }))
    }

    fn args(value: Value) -> Option<JsonObject> {
        Some(value.as_object().unwrap().clone())
    }

    #[test]
    fn a_default_fills_an_absent_property() {
        let prepared = bash().prepare(args(json!({ "command": "ls" }))).unwrap();
        assert_eq!(prepared["cwd"], json!("/w"));
    }

    #[test]
    fn a_supplied_value_beats_the_default() {
        let prepared = bash()
            .prepare(args(json!({ "command": "ls", "cwd": "/src" })))
            .unwrap();
        assert_eq!(prepared["cwd"], json!("/src"));
    }

    #[test]
    fn a_missing_required_property_is_rejected() {
        assert!(matches!(
            bash().prepare(args(json!({}))),
            Err(ToolError::Invalid(_))
        ));
    }

    #[test]
    fn a_wrongly_typed_property_is_rejected() {
        assert!(matches!(
            bash().prepare(args(json!({ "command": 7 }))),
            Err(ToolError::Invalid(_))
        ));
    }

    #[test]
    fn an_enum_is_enforced() {
        let s = schema(json!({
            "type": "object",
            "properties": { "mode": { "enum": ["fast", "slow"] } }
        }));
        assert!(s.prepare(args(json!({ "mode": "fast" }))).is_ok());
        assert!(matches!(
            s.prepare(args(json!({ "mode": "sideways" }))),
            Err(ToolError::Invalid(_))
        ));
    }

    #[test]
    fn absent_arguments_are_an_empty_object() {
        let s = schema(json!({ "type": "object" }));
        assert!(s.prepare(None).unwrap().is_empty());
    }

    #[test]
    fn every_declared_property_is_a_parameter() {
        let names: Vec<_> = bash().params().iter().cloned().collect();
        assert_eq!(names, ["command", "cwd"]);
    }

    #[test]
    fn a_malformed_schema_is_rejected_at_compile() {
        let object = json!({ "type": "not-a-type" }).as_object().unwrap().clone();
        assert!(matches!(
            InputSchema::compile("t", &object),
            Err(LoadError::Schema { .. })
        ));
    }
}
