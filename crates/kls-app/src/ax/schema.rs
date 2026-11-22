//! A small JSON Schema checker for the subset the contract's schemas use:
//! `type`, `const`, `enum`, `properties`, `required`,
//! `additionalProperties: false`, `items`, `$ref` to `#/$defs/…`, `anyOf`,
//! `not` and `if`/`then`/`else`. Every envelope is checked with it before
//! it is printed, and the tests check every line the CLI prints.

use serde_json::Value;

/// Checks `v` against `schema` (the root, for `$ref`).
pub fn validate(schema: &Value, v: &Value) -> Result<(), String> {
    check(schema, schema, v, "$")
}

fn type_ok(t: &str, v: &Value) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.is_number(),
        _ => false,
    }
}

fn check(root: &Value, s: &Value, v: &Value, at: &str) -> Result<(), String> {
    let Some(s) = s.as_object() else {
        return Ok(()); // `true` / `{}`
    };
    if let Some(r) = s.get("$ref").and_then(Value::as_str) {
        let def = r
            .strip_prefix("#/$defs/")
            .and_then(|name| root.get("$defs").and_then(|d| d.get(name)))
            .ok_or_else(|| format!("{at}: unresolved $ref {r}"))?;
        check(root, def, v, at)?;
    }
    match s.get("type") {
        Some(Value::String(t)) if !type_ok(t, v) => {
            return Err(format!("{at}: expected {t}"));
        }
        Some(Value::Array(ts)) if !ts.iter().filter_map(Value::as_str).any(|t| type_ok(t, v)) => {
            return Err(format!("{at}: expected one of {ts:?}"));
        }
        _ => {}
    }
    if let Some(c) = s.get("const")
        && c != v
    {
        return Err(format!("{at}: expected {c}"));
    }
    if let Some(Value::Array(e)) = s.get("enum")
        && !e.contains(v)
    {
        return Err(format!(
            "{at}: {v} is not one of {}",
            Value::Array(e.clone())
        ));
    }
    if let Some(o) = v.as_object() {
        if let Some(Value::Array(req)) = s.get("required") {
            for r in req.iter().filter_map(Value::as_str) {
                if !o.contains_key(r) {
                    return Err(format!("{at}: missing {r}"));
                }
            }
        }
        let props = s.get("properties").and_then(Value::as_object);
        for (k, val) in o {
            match props.and_then(|p| p.get(k)) {
                Some(ps) => check(root, ps, val, &format!("{at}.{k}"))?,
                None if s.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    return Err(format!("{at}: unexpected field {k}"));
                }
                None => {}
            }
        }
    }
    if let (Some(items), Some(a)) = (s.get("items"), v.as_array()) {
        for (i, x) in a.iter().enumerate() {
            check(root, items, x, &format!("{at}[{i}]"))?;
        }
    }
    if let Some(Value::Array(any)) = s.get("anyOf")
        && !any.iter().any(|x| check(root, x, v, at).is_ok())
    {
        return Err(format!("{at}: matches none of anyOf"));
    }
    if let Some(n) = s.get("not")
        && check(root, n, v, at).is_ok()
    {
        return Err(format!("{at}: matches a forbidden shape"));
    }
    if let Some(cond) = s.get("if") {
        let branch = if check(root, cond, v, at).is_ok() {
            s.get("then")
        } else {
            s.get("else")
        };
        if let Some(b) = branch {
            check(root, b, v, at)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn subset() {
        let s = json!({
            "type": "object", "required": ["a"], "additionalProperties": false,
            "properties": { "a": {"enum": [1, 2]}, "b": {"$ref": "#/$defs/b"} },
            "if": {"properties": {"a": {"const": 2}}}, "then": {"required": ["b"]},
            "$defs": { "b": {"type": ["string", "null"]} }
        });
        assert!(validate(&s, &json!({"a": 1})).is_ok());
        assert!(validate(&s, &json!({"a": 3})).is_err());
        assert!(validate(&s, &json!({"a": 2})).is_err());
        assert!(validate(&s, &json!({"a": 2, "b": null})).is_ok());
        assert!(validate(&s, &json!({"a": 2, "b": 5})).is_err());
        assert!(validate(&s, &json!({"a": 1, "c": 5})).is_err());
    }
}
