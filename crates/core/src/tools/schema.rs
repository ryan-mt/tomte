//! OpenAI strict-schema transformation for tool parameter schemas.
//! Split out of `tools`; logic unchanged.

use std::collections::BTreeSet;

use serde_json::Value;

/// OpenAI strict function schemas require every object property to be listed in
/// `required`. Fields that are optional at the Rust boundary are represented as
/// nullable instead of omitted.
pub(super) fn strict_parameters_schema(mut schema: Value) -> Value {
    stricten_schema_object(&mut schema);
    schema
}

fn stricten_schema_object(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(values) = obj.get_mut(key).and_then(Value::as_array_mut) {
            for value in values {
                stricten_schema_object(value);
            }
        }
    }
    if let Some(items) = obj.get_mut("items") {
        stricten_schema_object(items);
    }

    let is_object =
        schema_type_contains(obj.get("type"), "object") || obj.contains_key("properties");
    if !is_object {
        return;
    }

    let originally_required = obj
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();

    let property_names = obj
        .get("properties")
        .and_then(Value::as_object)
        .map(|props| props.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();

    if let Some(properties) = obj.get_mut("properties").and_then(Value::as_object_mut) {
        for (name, property) in properties {
            if !originally_required.contains(name) {
                mark_schema_nullable(property);
            }
            stricten_schema_object(property);
        }
    }

    obj.insert(
        "required".to_string(),
        Value::Array(property_names.into_iter().map(Value::String).collect()),
    );
    obj.entry("additionalProperties".to_string())
        .or_insert(Value::Bool(false));
}

pub(super) fn schema_type_contains(value: Option<&Value>, expected: &str) -> bool {
    match value {
        Some(Value::String(s)) => s == expected,
        Some(Value::Array(items)) => items.iter().any(|item| item.as_str() == Some(expected)),
        _ => false,
    }
}

fn mark_schema_nullable(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    match obj.get_mut("type") {
        Some(Value::String(ty)) if ty != "null" => {
            let ty = Value::String(std::mem::take(ty));
            obj.insert(
                "type".to_string(),
                Value::Array(vec![ty, Value::String("null".into())]),
            );
        }
        Some(Value::Array(types)) => {
            if !types.iter().any(|ty| ty.as_str() == Some("null")) {
                types.push(Value::String("null".into()));
            }
        }
        Some(_) => {}
        None => {
            obj.insert(
                "anyOf".to_string(),
                Value::Array(vec![
                    Value::Object(obj.clone()),
                    serde_json::json!({"type": "null"}),
                ]),
            );
        }
    }

    if let Some(values) = obj.get_mut("enum").and_then(Value::as_array_mut) {
        if !values.iter().any(Value::is_null) {
            values.push(Value::Null);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn required(schema: &Value) -> Vec<String> {
        schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn optional_props_become_nullable_and_every_prop_is_required() {
        // OpenAI strict mode: an optional field must be NULLABLE and ALSO listed
        // in `required` (a property absent from `required` is rejected), and
        // `additionalProperties` must be false.
        let out = strict_parameters_schema(json!({
            "type": "object",
            "properties": { "a": {"type": "string"}, "b": {"type": "number"} },
            "required": ["a"],
        }));
        assert_eq!(out["properties"]["a"]["type"], "string"); // required stays scalar
        assert_eq!(out["properties"]["b"]["type"], json!(["number", "null"]));
        let req = required(&out);
        assert!(req.contains(&"a".to_string()) && req.contains(&"b".to_string()));
        assert_eq!(out["additionalProperties"], false);
    }

    #[test]
    fn nested_objects_and_array_items_are_strictened_recursively() {
        let out = strict_parameters_schema(json!({
            "type": "object",
            "properties": {
                "cfg": { "type": "object", "properties": { "x": {"type": "string"} } },
                "list": { "type": "array", "items": { "type": "object",
                    "properties": { "y": {"type": "number"} } } },
            },
            "required": ["list"],
        }));
        // Optional nested object: nullable AND recursed (child nullable + required).
        assert_eq!(out["properties"]["cfg"]["type"], json!(["object", "null"]));
        assert_eq!(
            out["properties"]["cfg"]["properties"]["x"]["type"],
            json!(["string", "null"])
        );
        assert_eq!(out["properties"]["cfg"]["required"], json!(["x"]));
        assert_eq!(out["properties"]["cfg"]["additionalProperties"], false);
        // Array items object recursed even though `list` itself is required.
        assert_eq!(out["properties"]["list"]["type"], "array");
        assert_eq!(
            out["properties"]["list"]["items"]["properties"]["y"]["type"],
            json!(["number", "null"])
        );
        assert_eq!(out["properties"]["list"]["items"]["required"], json!(["y"]));
    }

    #[test]
    fn optional_enum_gains_a_null_variant() {
        let out = strict_parameters_schema(json!({
            "type": "object",
            "properties": { "mode": {"type": "string", "enum": ["a", "b"]} },
        }));
        assert_eq!(out["properties"]["mode"]["type"], json!(["string", "null"]));
        let variants = out["properties"]["mode"]["enum"].as_array().unwrap();
        assert!(
            variants.iter().any(Value::is_null),
            "a nullable enum must allow null: {variants:?}"
        );
    }
}
