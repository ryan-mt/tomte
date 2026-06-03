//! Argument parsing helpers shared by tools: the typed `parse_args`
//! wrapper and the lenient scalar/list deserializers. Split out of `tools`;
//! logic unchanged. Re-exported from `tools` so the `crate::tools::deserialize_*`
//! paths used in `#[serde(deserialize_with = ...)]` attributes still resolve.

use serde::Deserialize;

use super::ArgSchemaError;

/// Deserialize a tool's `Value` arguments into the tool's typed struct. A
/// failure is wrapped in [`ArgSchemaError`] (whose `Display` is the familiar
/// `tool <name> argument schema mismatch: …`) so the agent can tell an
/// argument-shape error apart from a runtime error and append a schema hint for
/// the model to self-correct from.
pub fn parse_args<T: serde::de::DeserializeOwned>(
    tool: &str,
    args: serde_json::Value,
) -> anyhow::Result<T> {
    serde_json::from_value::<T>(args).map_err(|e| {
        anyhow::Error::new(ArgSchemaError {
            tool: tool.to_string(),
            detail: e.to_string(),
        })
    })
}

pub fn deserialize_bool<'de, D>(deserializer: D) -> std::result::Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(
        parse_optional_bool_value(Some(serde_json::Value::deserialize(deserializer)?))?
            .unwrap_or(false),
    )
}

pub fn deserialize_optional_bool<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    parse_optional_bool_value(value)
}

pub fn deserialize_optional_usize<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(value) = Option::<serde_json::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let Some(n) = parse_optional_u64_value(value)? else {
        return Ok(None);
    };
    usize::try_from(n)
        .map(Some)
        .map_err(|_| serde::de::Error::custom("integer is too large for this platform"))
}

pub fn deserialize_optional_u64<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(value) = Option::<serde_json::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    parse_optional_u64_value(value)
}

pub fn deserialize_optional_string_vec<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(value) = Option::<serde_json::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Array(items) => {
            let mut out = Vec::new();
            for item in items {
                match item {
                    serde_json::Value::String(s) => {
                        let trimmed = s.trim();
                        if !trimmed.is_empty() {
                            out.push(trimmed.to_string());
                        }
                    }
                    _ => return Err(serde::de::Error::custom("expected string array")),
                }
            }
            Ok(if out.is_empty() { None } else { Some(out) })
        }
        serde_json::Value::String(s) => {
            let out = split_string_list(&s);
            Ok(if out.is_empty() { None } else { Some(out) })
        }
        _ => Err(serde::de::Error::custom(
            "expected string, string array, or null",
        )),
    }
}

fn parse_optional_bool_value<E: serde::de::Error>(
    value: Option<serde_json::Value>,
) -> std::result::Result<Option<bool>, E> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Bool(b) => Ok(Some(b)),
        serde_json::Value::Number(n) => match n.as_u64() {
            Some(0) => Ok(Some(false)),
            Some(1) => Ok(Some(true)),
            _ => Err(E::custom("expected boolean or 0/1")),
        },
        serde_json::Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "" | "null" => Ok(None),
            "true" | "1" | "yes" => Ok(Some(true)),
            "false" | "0" | "no" => Ok(Some(false)),
            _ => Err(E::custom("expected boolean string")),
        },
        _ => Err(E::custom("expected boolean, boolean string, 0/1, or null")),
    }
}

fn parse_optional_u64_value<E: serde::de::Error>(
    value: serde_json::Value,
) -> std::result::Result<Option<u64>, E> {
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(n) => n
            .as_u64()
            .map(Some)
            .ok_or_else(|| E::custom("expected a non-negative integer")),
        serde_json::Value::String(s) => match s.trim() {
            "" | "null" => Ok(None),
            trimmed => trimmed
                .parse::<u64>()
                .map(Some)
                .map_err(|_| E::custom("expected a non-negative integer")),
        },
        _ => Err(E::custom("expected an integer, integer string, or null")),
    }
}

fn split_string_list(value: &str) -> Vec<String> {
    value
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod scalar_arg_tests {
    use crate::tools::TodoStatus;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Deserialize)]
    struct Args {
        #[serde(default, deserialize_with = "crate::tools::deserialize_bool")]
        flag: bool,
        #[serde(default, deserialize_with = "crate::tools::deserialize_optional_bool")]
        maybe_flag: Option<bool>,
        #[serde(default, deserialize_with = "crate::tools::deserialize_optional_usize")]
        count: Option<usize>,
        #[serde(default, deserialize_with = "crate::tools::deserialize_optional_u64")]
        bytes: Option<u64>,
        #[serde(
            default,
            deserialize_with = "crate::tools::deserialize_optional_string_vec"
        )]
        list: Option<Vec<String>>,
    }

    #[test]
    fn semantic_scalar_deserializers_accept_common_model_spellings() {
        let args: Args = serde_json::from_value(json!({
            "flag": "yes",
            "maybe_flag": 0,
            "count": "42",
            "bytes": "9000",
            "list": "docs.rs, crates.io example.com"
        }))
        .unwrap();

        assert!(args.flag);
        assert_eq!(args.maybe_flag, Some(false));
        assert_eq!(args.count, Some(42));
        assert_eq!(args.bytes, Some(9000));
        assert_eq!(
            args.list,
            Some(vec![
                "docs.rs".to_string(),
                "crates.io".to_string(),
                "example.com".to_string()
            ])
        );
    }

    #[test]
    fn semantic_scalar_deserializers_treat_blank_and_null_as_none() {
        let args: Args = serde_json::from_value(json!({
            "flag": null,
            "maybe_flag": "",
            "count": "null",
            "bytes": null,
            "list": ""
        }))
        .unwrap();

        assert!(!args.flag);
        assert_eq!(args.maybe_flag, None);
        assert_eq!(args.count, None);
        assert_eq!(args.bytes, None);
        assert_eq!(args.list, None);
    }

    #[test]
    fn todo_status_parse_accepts_common_model_spellings() {
        assert_eq!(TodoStatus::parse("pending"), Some(TodoStatus::Pending));
        assert_eq!(TodoStatus::parse("todo"), Some(TodoStatus::Pending));
        assert_eq!(
            TodoStatus::parse("in progress"),
            Some(TodoStatus::InProgress)
        );
        assert_eq!(TodoStatus::parse("active"), Some(TodoStatus::InProgress));
        assert_eq!(TodoStatus::parse("done"), Some(TodoStatus::Completed));
        assert_eq!(TodoStatus::parse("complete"), Some(TodoStatus::Completed));
    }
}
