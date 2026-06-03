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
