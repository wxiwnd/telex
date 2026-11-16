use serde_json::Value;
use sha2::{Digest, Sha256};

/// Canonical JSON: object keys sorted by code point, no whitespace, UTF-8.
/// v0.1 is JCS-lite; values are limited to ASCII, numbers, enums and base64.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, &mut out);
    out
}

fn write_value(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => out.push_str(&serde_json::to_string(s).expect("string serializes")),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).expect("key serializes"));
                out.push(':');
                write_value(&map[*key], out);
            }
            out.push('}');
        }
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sorts_keys_and_keeps_compact_shape() {
        let value = json!({"b": 1, "a": [true, null], "c": "x"});
        assert_eq!(canonical_json(&value), r#"{"a":[true,null],"b":1,"c":"x"}"#);
    }

    #[test]
    fn nested_objects_are_sorted() {
        let value = json!({"z": {"b": 2, "a": 1}, "a": 1});
        assert_eq!(canonical_json(&value), r#"{"a":1,"z":{"a":1,"b":2}}"#);
    }

    #[test]
    fn hash_is_stable_for_reordered_input() {
        let a = json!({"x": 1, "y": {"b": 2, "a": 3}});
        let b = json!({"y": {"a": 3, "b": 2}, "x": 1});
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(
            sha256_hex(canonical_json(&a).as_bytes()),
            sha256_hex(canonical_json(&b).as_bytes())
        );
    }
}
