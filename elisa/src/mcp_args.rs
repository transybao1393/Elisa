//! Clean-up of a model's MCP tool arguments before they are sent (`mcp_host.rs` `Host::call`). Small models fill
//! optional fields with placeholders (`"revisesThought": 0, "branchId": ""`) and a strict server then rejects the
//! whole call. Using the tool's own `inputSchema` (kept from `tools/list`), an optional property (not in `required`)
//! is dropped when its value is empty (null, "", [], {}) or breaks that property's own schema (type, minimum /
//! exclusiveMinimum / maximum / exclusiveMaximum, minLength / maxLength, enum, const). Required fields are never
//! touched, and a value that satisfies its schema is always kept (0 stays when 0 is allowed).
use serde_json::{Map, Value};

/// `args` with the bad optional fields left out, and the names of the fields dropped (in argument order).
pub fn clean(schema: &Value, args: &Value) -> (Value, Vec<String>) {
    let Some(obj) = args.as_object() else { return (args.clone(), Vec::new()) };
    let props = schema.get("properties").and_then(Value::as_object);
    let required: Vec<&str> = schema.get("required").and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let mut out = Map::new();
    let mut dropped = Vec::new();
    for (k, v) in obj {
        let keep = required.contains(&k.as_str()) || (!empty(v) && props.and_then(|p| p.get(k)).is_none_or(|s| fits(s, v)));
        if keep { out.insert(k.clone(), v.clone()); } else { dropped.push(k.clone()); }
    }
    (Value::Object(out), dropped)
}

fn empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

fn type_ok(t: &str, v: &Value) -> bool {
    match t {
        "string" => v.is_string(),
        "number" => v.is_number(),
        "integer" => v.is_i64() || v.is_u64() || v.as_f64().is_some_and(|f| f.fract() == 0.0),
        "boolean" => v.is_boolean(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        "null" => v.is_null(),
        _ => true, // a type this check does not know: not judged
    }
}

/// `v` meets the property schema's own simple constraints. Anything this check does not understand (anyOf, $ref,
/// patterns, nested items) counts as met: only a clear violation drops a field.
fn fits(s: &Value, v: &Value) -> bool {
    let Some(s) = s.as_object() else { return true };
    match s.get("type") {
        Some(Value::String(t)) if !type_ok(t, v) => return false,
        Some(Value::Array(ts)) if !ts.iter().filter_map(Value::as_str).any(|t| type_ok(t, v)) => return false,
        _ => {}
    }
    if let Some(e) = s.get("enum").and_then(Value::as_array) { if !e.contains(v) { return false; } }
    if let Some(c) = s.get("const") { if c != v { return false; } }
    if let Some(x) = v.as_f64() {
        let n = |k: &str| s.get(k).and_then(Value::as_f64);
        // draft 4 wrote exclusiveMinimum / exclusiveMaximum as booleans next to minimum / maximum
        let excl = |k: &str| s.get(k).and_then(Value::as_bool).unwrap_or(false);
        if let Some(m) = n("minimum") { if x < m || (excl("exclusiveMinimum") && x == m) { return false; } }
        if let Some(m) = n("maximum") { if x > m || (excl("exclusiveMaximum") && x == m) { return false; } }
        if let Some(m) = n("exclusiveMinimum") { if x <= m { return false; } }
        if let Some(m) = n("exclusiveMaximum") { if x >= m { return false; } }
    }
    if let Some(t) = v.as_str() {
        let len = t.chars().count() as u64;
        if s.get("minLength").and_then(Value::as_u64).is_some_and(|m| len < m) { return false; }
        if s.get("maxLength").and_then(Value::as_u64).is_some_and(|m| len > m) { return false; }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// the real `sequentialthinking` schema (@modelcontextprotocol/server-sequential-thinking, descriptions left out)
    fn seq() -> Value {
        let int1 = json!({"type": "integer", "minimum": 1, "maximum": 9007199254740991_u64});
        let bs = json!({"type": ["boolean", "string"]});
        json!({"type": "object", "$schema": "https://json-schema.org/draft/2020-12/schema",
            "properties": {"thought": {"type": "string"}, "nextThoughtNeeded": bs, "thoughtNumber": int1, "totalThoughts": int1,
                "isRevision": bs, "revisesThought": int1, "branchFromThought": int1, "branchId": {"type": "string"}, "needsMoreThoughts": bs},
            "required": ["thought", "nextThoughtNeeded", "thoughtNumber", "totalThoughts"]})
    }

    #[test]
    fn the_sequential_thinking_placeholders_are_dropped() {
        // the 2B model's rejected call: `revisesThought: Too small: expected number to be >=1`
        let args = json!({"thought": "list the files", "nextThoughtNeeded": true, "thoughtNumber": 1, "totalThoughts": 3,
            "isRevision": false, "revisesThought": 0, "branchFromThought": 0, "branchId": "", "needsMoreThoughts": false});
        let (out, dropped) = clean(&seq(), &args);
        assert_eq!(dropped, ["revisesThought", "branchFromThought", "branchId"]);
        assert_eq!(out, json!({"thought": "list the files", "nextThoughtNeeded": true, "thoughtNumber": 1, "totalThoughts": 3,
            "isRevision": false, "needsMoreThoughts": false}));
        // valid optional values stay; required ones are never touched, even when wrong (the server says why)
        let (out, dropped) = clean(&seq(), &json!({"thought": "", "nextThoughtNeeded": "yes", "thoughtNumber": 0, "totalThoughts": 2, "revisesThought": 1, "branchId": "b"}));
        assert!(dropped.is_empty());
        assert_eq!(out["thoughtNumber"], 0);
        assert_eq!(out["revisesThought"], 1);
    }

    #[test]
    fn optional_fields_go_only_when_empty_or_against_their_schema() {
        let s = json!({"type": "object", "required": ["q"], "properties": {
            "q": {"type": "string"}, "n": {"type": "integer", "minimum": 0}, "k": {"type": "number", "exclusiveMinimum": 0},
            "mode": {"enum": ["fast", "slow"]}, "tag": {"type": "string", "minLength": 2}, "flag": {"type": "boolean"},
            "old": {"type": "integer", "minimum": 5, "exclusiveMinimum": true}, "list": {"type": "array"}, "free": {}}});
        let (out, dropped) = clean(&s, &json!({"q": "", "n": 0, "k": 0, "mode": "medium", "tag": "x", "flag": "no",
            "old": 5, "list": [], "free": {}, "extra": null, "other": 3}));
        assert_eq!(dropped, ["k", "mode", "tag", "flag", "old", "list", "free", "extra"]);
        // `q` is required (kept even empty), n = 0 meets minimum 0, `other` is not in the schema and not empty
        assert_eq!(out, json!({"q": "", "n": 0, "other": 3}));
        let (out, dropped) = clean(&s, &json!({"q": "a", "n": 2.0, "k": 0.5, "mode": "fast", "tag": "ab", "flag": false, "old": 6, "list": [1]}));
        assert!(dropped.is_empty(), "{dropped:?}");
        assert_eq!(out["n"], 2.0);
        // n: a float that is not whole is not an integer
        assert_eq!(clean(&s, &json!({"q": "a", "n": 1.5})).1, ["n"]);
    }

    #[test]
    fn no_schema_or_no_object_changes_only_empty_optionals() {
        let (out, dropped) = clean(&json!({}), &json!({"a": 0, "b": "", "c": false}));
        assert_eq!((out, dropped), (json!({"a": 0, "c": false}), vec!["b".to_string()]));
        assert_eq!(clean(&json!({}), &json!("x")).0, json!("x"));
    }
}
