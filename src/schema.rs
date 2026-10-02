//! Make a schemars-generated JSON schema acceptable to Claude's structured outputs.
//!
//! A port of the Python SDK's `transform_schema`: every object gets `additionalProperties: false`
//! and all of its properties required; keywords the API doesn't support (numeric bounds, most
//! formats) are moved into the description so the model still sees them. Rust-specific extras:
//! nullable `{"type": [T, "null"]}` becomes `anyOf`, and numeric formats like "uint8" are dropped.

use serde_json::{Map, Value, json};

const STRING_FORMATS: &[&str] =
    &["date-time", "time", "date", "duration", "email", "hostname", "uri", "ipv4", "ipv6", "uuid"];

pub fn strict(schema: &Value) -> Value {
    let mut src = schema.as_object().cloned().unwrap_or_default();
    src.remove("$schema");
    let mut out = Map::new();

    if let Some(Value::Object(defs)) = src.remove("$defs") {
        out.insert("$defs".into(), Value::Object(defs.iter().map(|(k, v)| (k.clone(), strict(v))).collect()));
    }
    if let Some(r) = src.remove("$ref") {
        out.insert("$ref".into(), r);
        return Value::Object(out);
    }

    if let Some(Value::Array(types)) = src.get("type").cloned() {
        src.remove("type");
        let description = src.remove("description");
        let variants: Vec<Value> = types
            .iter()
            .map(|t| {
                if t == "null" {
                    json!({"type": "null"})
                } else {
                    let mut v = src.clone();
                    v.insert("type".into(), t.clone());
                    strict(&Value::Object(v))
                }
            })
            .collect();
        out.insert("anyOf".into(), Value::Array(variants));
        if let Some(d) = description {
            out.insert("description".into(), d);
        }
        return Value::Object(out);
    }

    let type_ = src.remove("type");
    let any_of = src.remove("anyOf").or_else(|| src.remove("oneOf"));
    if let Some(Value::Array(variants)) = any_of {
        out.insert("anyOf".into(), Value::Array(variants.iter().map(strict).collect()));
    } else if let Some(Value::Array(variants)) = src.remove("allOf") {
        out.insert("allOf".into(), Value::Array(variants.iter().map(strict).collect()));
    } else if let Some(t) = &type_ {
        out.insert("type".into(), t.clone());
    }

    for key in ["enum", "description", "title"] {
        if let Some(v) = src.remove(key) {
            out.insert(key.into(), v);
        }
    }

    match type_.as_ref().and_then(Value::as_str) {
        Some("object") => {
            let props = match src.remove("properties") {
                Some(Value::Object(p)) => p,
                _ => Map::new(),
            };
            let required: Vec<Value> = props.keys().map(|k| Value::String(k.clone())).collect();
            out.insert("properties".into(), Value::Object(props.iter().map(|(k, v)| (k.clone(), strict(v))).collect()));
            src.remove("additionalProperties");
            src.remove("required");
            out.insert("additionalProperties".into(), Value::Bool(false));
            out.insert("required".into(), Value::Array(required));
        }
        Some("string") => {
            if let Some(f) = src.remove("format") {
                if f.as_str().is_some_and(|f| STRING_FORMATS.contains(&f)) {
                    out.insert("format".into(), f);
                } else {
                    src.insert("format".into(), f);
                }
            }
        }
        Some("array") => {
            if let Some(items) = src.remove("items") {
                out.insert("items".into(), strict(&items));
            }
            if let Some(min) = src.remove("minItems") {
                if min == 0 || min == 1 {
                    out.insert("minItems".into(), min);
                } else {
                    src.insert("minItems".into(), min);
                }
            }
        }
        Some("integer" | "number") => {
            src.remove("format"); // "uint8", "double", ...: meaningless to the model
        }
        _ => {}
    }

    // Anything left is unsupported: tell the model in the description instead.
    if !src.is_empty() {
        let extras = src.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join(", ");
        let description = match out.get("description").and_then(Value::as_str) {
            Some(d) => format!("{d}\n\n{{{extras}}}"),
            None => format!("{{{extras}}}"),
        };
        out.insert("description".into(), Value::String(description));
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nullable_bounded_integer_becomes_any_of_with_bounds_in_description() {
        let s = strict(&json!({
            "type": "object",
            "properties": {"score": {"type": ["integer", "null"], "format": "uint8", "minimum": 0, "maximum": 255,
                                     "description": "1-5"}},
        }));
        assert_eq!(s["additionalProperties"], false);
        assert_eq!(s["required"], json!(["score"]));
        let score = &s["properties"]["score"];
        assert_eq!(score["description"], "1-5");
        assert_eq!(score["anyOf"][1], json!({"type": "null"}));
        assert_eq!(score["anyOf"][0]["type"], "integer");
        assert!(score["anyOf"][0]["description"].as_str().unwrap().contains("maximum: 255"));
        assert!(score["anyOf"][0].get("format").is_none());
    }

    #[test]
    fn analysis_schema_is_strict_everywhere() {
        let s = strict(&serde_json::to_value(schemars::schema_for!(crate::models::SessionAnalysis)).unwrap());
        fn check(v: &Value) {
            if let Some(o) = v.as_object() {
                if o.get("type") == Some(&json!("object")) {
                    assert_eq!(o["additionalProperties"], false, "{v}");
                    let props = o["properties"].as_object().unwrap();
                    assert_eq!(o["required"].as_array().unwrap().len(), props.len(), "{v}");
                }
                assert!(!o.contains_key("minimum") && !o.contains_key("maximum"), "{v}");
                assert!(!matches!(o.get("type"), Some(Value::Array(_))), "{v}");
                o.values().for_each(check);
            } else if let Some(a) = v.as_array() {
                a.iter().for_each(check);
            }
        }
        check(&s);
        assert!(s["$defs"]["Verdict"]["enum"].as_array().unwrap().contains(&json!("leaning_positive")));
    }
}
