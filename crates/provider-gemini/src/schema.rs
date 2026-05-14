//! JSON Schema → Gemini OpenAPI-subset sanitizer.
//!
//! Tool input schemas reach this provider as JSON Schema (typically emitted
//! by `schemars` on the MCP-tool side). Gemini's `function_declarations[].
//! parameters` field is parsed by a protobuf schema that only understands a
//! subset of OpenAPI 3.0 and will reject unknown keywords with errors like
//! `Cannot find field "$schema"` or `Proto field is not repeating, cannot
//! start list` (for `type: ["X", "null"]`).
//!
//! This module rewrites a schema into Gemini's accepted shape:
//!
//! - Inlines every `$ref: "#/$defs/Foo"` or `#/definitions/Foo` against the
//!   root's `$defs` / `definitions` map, then drops the maps themselves.
//! - Drops JSON-Schema metadata that Gemini rejects: `$schema`, `$id`,
//!   `$comment`, `additionalProperties`, `unevaluatedProperties`,
//!   `patternProperties`.
//! - Converts `type: ["X", "null"]` (the `Option<X>` shape from `schemars`)
//!   into `type: "X"` plus `nullable: true`. Multi-non-null arrays collapse
//!   to the first non-null entry — Gemini does not support union types.
//! - Converts `const: X` into `enum: [X]` (Gemini has no `const` keyword
//!   but does accept `enum`, so the discriminator constraint is preserved).
//! - Renames `oneOf` to `anyOf` (Gemini accepts only `anyOf`).
//! - In an `anyOf`, strips bare `{"type": "null"}` members and lifts them
//!   to `nullable: true` on the enclosing schema. This is the standard
//!   schemars encoding for nullable refs (`anyOf: [{$ref}, {type:null}]`)
//!   and Gemini's type enum has no `"null"` value.
//!
//! Other keywords (`properties`, `items`, `required`, `enum`, `description`,
//! `format`, `nullable`, `anyOf`, ...) pass through unchanged.

use serde_json::{Map, Value};

/// Cap on `$ref` resolution depth. Protects against recursive schemas
/// (a type referencing itself) and against pathological `$defs` chains.
/// Beyond this depth the offending node is replaced with `Value::Null`,
/// which serializes to `null` and is at least valid JSON.
const MAX_DEPTH: u32 = 32;

/// Rewrite a JSON Schema into the Gemini-accepted OpenAPI subset.
///
/// `schema` is expected to be the root of a single tool's `inputSchema`.
/// Returns a freshly-allocated `Value`; the input is not mutated.
pub(crate) fn sanitize(schema: &Value) -> Value {
    let defs = extract_defs(schema);
    let mut out = walk(schema, &defs, 0);
    if let Some(map) = out.as_object_mut() {
        map.remove("$schema");
        map.remove("$defs");
        map.remove("definitions");
        map.remove("$id");
        map.remove("$comment");
    }
    out
}

fn extract_defs(schema: &Value) -> Map<String, Value> {
    let mut defs = Map::new();
    let Some(obj) = schema.as_object() else {
        return defs;
    };
    if let Some(Value::Object(m)) = obj.get("$defs") {
        for (k, v) in m {
            defs.insert(k.clone(), v.clone());
        }
    }
    if let Some(Value::Object(m)) = obj.get("definitions") {
        for (k, v) in m {
            defs.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    defs
}

fn walk(value: &Value, defs: &Map<String, Value>, depth: u32) -> Value {
    if depth > MAX_DEPTH {
        return Value::Null;
    }
    match value {
        Value::Object(map) => walk_object(map, defs, depth),
        Value::Array(arr) => Value::Array(arr.iter().map(|v| walk(v, defs, depth + 1)).collect()),
        other => other.clone(),
    }
}

fn walk_object(map: &Map<String, Value>, defs: &Map<String, Value>, depth: u32) -> Value {
    // `$ref` with sibling keywords: the resolved target wins, and any
    // sibling keys merge on top only when the target doesn't already
    // define them. This matches how most JSON-Schema-aware tools treat
    // ref-with-siblings while keeping the output minimal.
    if let Some(Value::String(reference)) = map.get("$ref") {
        let target_name = reference
            .strip_prefix("#/$defs/")
            .or_else(|| reference.strip_prefix("#/definitions/"));
        if let Some(name) = target_name
            && let Some(target) = defs.get(name)
        {
            let mut resolved = walk(target, defs, depth + 1);
            if let Some(resolved_map) = resolved.as_object_mut() {
                for (k, v) in map {
                    if k == "$ref" {
                        continue;
                    }
                    resolved_map
                        .entry(k.clone())
                        .or_insert_with(|| walk(v, defs, depth + 1));
                }
            }
            return resolved;
        }
        // Unresolvable ref (external URL, missing def). Drop the $ref
        // and process the remaining keywords — Gemini gets at least a
        // best-effort schema instead of a hard error on `Unknown name "$ref"`.
        let mut without_ref = map.clone();
        without_ref.remove("$ref");
        return walk_object(&without_ref, defs, depth);
    }

    let mut out = Map::with_capacity(map.len());
    for (key, value) in map {
        match key.as_str() {
            "$schema" | "$id" | "$comment" | "$defs" | "definitions"
            | "additionalProperties" | "unevaluatedProperties" | "patternProperties" => continue,
            "type" => {
                rewrite_type(value, &mut out);
            }
            "const" => {
                // Gemini has no `const`. A single-element `enum` carries the
                // same constraint and is part of the accepted subset.
                out.insert("enum".to_string(), Value::Array(vec![value.clone()]));
            }
            "oneOf" => {
                out.insert("anyOf".to_string(), walk(value, defs, depth + 1));
            }
            _ => {
                out.insert(key.clone(), walk(value, defs, depth + 1));
            }
        }
    }
    lift_null_from_any_of(&mut out);
    Value::Object(out)
}

/// If `out["anyOf"]` contains any `{"type": "null"}` members, drop them and
/// set `nullable: true` on `out`. When only one member is left, collapse
/// the schema into the parent (merging its keys, preferring existing
/// parent keys) — Gemini's tool-parameter parser tolerates `anyOf` with
/// multiple objects but balks at a one-member `anyOf` wrapping an obvious
/// scalar.
fn lift_null_from_any_of(out: &mut Map<String, Value>) {
    let nullable;
    let remaining_len;
    {
        let Some(Value::Array(arr)) = out.get_mut("anyOf") else {
            return;
        };
        let before = arr.len();
        arr.retain(|item| {
            let Some(m) = item.as_object() else {
                return true;
            };
            let is_bare_null = m.get("type") == Some(&Value::String("null".into()))
                && m.keys().all(|k| k == "type");
            !is_bare_null
        });
        nullable = arr.len() != before;
        remaining_len = arr.len();
    }
    if nullable {
        out.entry("nullable".to_string())
            .or_insert(Value::Bool(true));
    }
    match remaining_len {
        0 => {
            // Empty after stripping null — drop `anyOf`; `nullable: true`
            // (if set) carries the remaining semantics.
            out.remove("anyOf");
        }
        1 => {
            // Collapse single-member `anyOf` into the parent. Take the
            // owned value, then merge its keys without overwriting any
            // sibling already on `out` (e.g. `description`).
            let single = match out.remove("anyOf") {
                Some(Value::Array(mut arr)) => arr.remove(0),
                _ => return,
            };
            if let Value::Object(single_map) = single {
                for (k, v) in single_map {
                    out.entry(k).or_insert(v);
                }
            }
        }
        _ => {}
    }
}

fn rewrite_type(value: &Value, out: &mut Map<String, Value>) {
    let Value::Array(arr) = value else {
        out.insert("type".to_string(), value.clone());
        return;
    };
    // schemars emits `["X", "null"]` for `Option<X>`. Pull out the
    // nullability flag, then take the first remaining type as the
    // canonical one. Gemini's schema enforces single-typed properties,
    // so any second non-null type would be silently lost on the wire
    // anyway; we drop it here so the failure mode is local.
    let mut chosen: Option<&Value> = None;
    let mut nullable = false;
    for t in arr {
        match t {
            Value::String(s) if s == "null" => nullable = true,
            _ if chosen.is_none() => chosen = Some(t),
            _ => {}
        }
    }
    if let Some(t) = chosen {
        out.insert("type".to_string(), t.clone());
    }
    if nullable {
        out.insert("nullable".to_string(), Value::Bool(true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_dollar_schema_at_root() {
        let input = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {}
        });
        let out = sanitize(&input);
        assert!(out.get("$schema").is_none(), "out: {out}");
        assert_eq!(out["type"], json!("object"));
    }

    #[test]
    fn nullable_from_option_type_array() {
        let input = json!({
            "type": "object",
            "properties": {
                "name": { "type": ["string", "null"] }
            }
        });
        let out = sanitize(&input);
        let name = &out["properties"]["name"];
        assert_eq!(name["type"], json!("string"));
        assert_eq!(name["nullable"], json!(true));
    }

    #[test]
    fn null_first_in_array_still_works() {
        let input = json!({ "type": ["null", "integer"] });
        let out = sanitize(&input);
        assert_eq!(out["type"], json!("integer"));
        assert_eq!(out["nullable"], json!(true));
    }

    #[test]
    fn ref_is_inlined_and_defs_dropped() {
        let input = json!({
            "type": "object",
            "properties": {
                "user": { "$ref": "#/$defs/User" }
            },
            "$defs": {
                "User": {
                    "type": "object",
                    "properties": { "id": { "type": "string" } }
                }
            }
        });
        let out = sanitize(&input);
        assert!(out.get("$defs").is_none(), "out: {out}");
        let user = &out["properties"]["user"];
        assert_eq!(user["type"], json!("object"));
        assert_eq!(user["properties"]["id"]["type"], json!("string"));
    }

    #[test]
    fn ref_inside_any_of_is_inlined_and_optional_collapses() {
        // schemars emits this exact shape for `Option<A>` where `A` is a
        // user-defined struct. After sanitization the ref is inlined, the
        // `type: null` member is lifted to `nullable: true`, and the
        // remaining single-member `anyOf` collapses into the parent.
        let input = json!({
            "type": "object",
            "properties": {
                "v": {
                    "anyOf": [
                        { "$ref": "#/$defs/A" },
                        { "type": "null" }
                    ]
                }
            },
            "$defs": {
                "A": { "type": "string" }
            }
        });
        let out = sanitize(&input);
        let v = &out["properties"]["v"];
        assert!(v.get("anyOf").is_none(), "anyOf should have collapsed: {v}");
        assert!(v.get("$ref").is_none());
        assert_eq!(v["type"], json!("string"));
        assert_eq!(v["nullable"], json!(true));
    }

    #[test]
    fn one_of_renamed_to_any_of() {
        let input = json!({ "oneOf": [{ "type": "string" }, { "type": "integer" }] });
        let out = sanitize(&input);
        assert!(out.get("oneOf").is_none());
        assert!(out["anyOf"].is_array());
    }

    #[test]
    fn unresolvable_ref_is_dropped_not_kept() {
        let input = json!({
            "type": "object",
            "properties": {
                "x": { "$ref": "https://example.com/foo.json" }
            }
        });
        let out = sanitize(&input);
        assert!(out["properties"]["x"].get("$ref").is_none());
    }

    #[test]
    fn ref_with_sibling_description_preserves_sibling() {
        let input = json!({
            "type": "object",
            "properties": {
                "user": {
                    "$ref": "#/$defs/U",
                    "description": "the user"
                }
            },
            "$defs": { "U": { "type": "object" } }
        });
        let out = sanitize(&input);
        let user = &out["properties"]["user"];
        assert_eq!(user["type"], json!("object"));
        assert_eq!(user["description"], json!("the user"));
    }

    #[test]
    fn recursive_ref_terminates_at_depth_cap() {
        let input = json!({
            "$defs": { "Self": { "$ref": "#/$defs/Self" } },
            "$ref": "#/$defs/Self"
        });
        // Must not blow the stack. We don't care what the output looks
        // like as long as the function returns.
        let _ = sanitize(&input);
    }

    #[test]
    fn additional_properties_is_dropped() {
        let input = json!({
            "type": "object",
            "properties": { "x": { "type": "integer" } },
            "additionalProperties": false
        });
        let out = sanitize(&input);
        assert!(out.get("additionalProperties").is_none(), "out: {out}");
    }

    #[test]
    fn const_is_rewritten_to_enum() {
        let input = json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "const": "replace" }
            }
        });
        let out = sanitize(&input);
        let op = &out["properties"]["op"];
        assert!(op.get("const").is_none());
        assert_eq!(op["enum"], json!(["replace"]));
        assert_eq!(op["type"], json!("string"));
    }

    #[test]
    fn any_of_with_type_null_lifts_to_nullable_and_collapses() {
        // schemars emits this exact shape for `Option<SomeRef>`.
        let input = json!({
            "type": "object",
            "properties": {
                "count": {
                    "description": "Match-count contract.",
                    "anyOf": [
                        { "$ref": "#/$defs/ReplaceCount" },
                        { "type": "null" }
                    ]
                }
            },
            "$defs": {
                "ReplaceCount": { "type": "string", "enum": ["all"] }
            }
        });
        let out = sanitize(&input);
        let count = &out["properties"]["count"];
        assert!(count.get("anyOf").is_none(), "anyOf should have collapsed: {count}");
        assert_eq!(count["nullable"], json!(true));
        assert_eq!(count["type"], json!("string"));
        assert_eq!(count["enum"], json!(["all"]));
        assert_eq!(count["description"], json!("Match-count contract."));
    }

    #[test]
    fn any_of_with_multiple_real_members_keeps_any_of() {
        let input = json!({
            "anyOf": [
                { "type": "string" },
                { "type": "integer" },
                { "type": "null" }
            ]
        });
        let out = sanitize(&input);
        let any_of = out["anyOf"].as_array().expect("anyOf preserved");
        assert_eq!(any_of.len(), 2);
        assert_eq!(out["nullable"], json!(true));
    }

    #[test]
    fn schemars_multi_edit_fixture_round_trips_to_gemini_subset() {
        // Excerpted from a real `tool-fs` `multi_edit` `inputSchema`. The
        // shape exercises every transform: $ref into a oneOf of object
        // variants, const-discriminated tags, additionalProperties: false
        // on an inner variant, anyOf-with-null for an optional ref, and
        // $defs at the root.
        let input = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "edits": {
                    "type": "array",
                    "items": { "$ref": "#/$defs/MultiEdit" }
                }
            },
            "required": ["edits"],
            "$defs": {
                "MultiEdit": {
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": {
                                "count": {
                                    "anyOf": [
                                        { "$ref": "#/$defs/ReplaceCount" },
                                        { "type": "null" }
                                    ]
                                },
                                "op": { "type": "string", "const": "replace" }
                            },
                            "required": ["op"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "op": { "type": "string", "const": "insert" }
                            },
                            "required": ["op"]
                        }
                    ]
                },
                "ReplaceCount": {
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": { "exactly": { "type": "integer" } },
                            "required": ["exactly"],
                            "additionalProperties": false
                        },
                        { "type": "string", "const": "all" }
                    ]
                }
            }
        });

        let out = sanitize(&input);

        // Every keyword Gemini rejects must be gone, recursively.
        assert_no_forbidden_keys(&out);

        // Functional checks on a couple of representative leaves.
        let edit_variants = out["properties"]["edits"]["items"]["anyOf"]
            .as_array()
            .expect("MultiEdit inlined into items.anyOf");
        assert_eq!(edit_variants.len(), 2);
        // `op` discriminator survives as `enum: ["replace"]`.
        assert_eq!(
            edit_variants[0]["properties"]["op"]["enum"],
            json!(["replace"])
        );
        // Inlined ReplaceCount in the replace variant: `count` collapsed
        // out of its null-bearing anyOf to a single nullable schema that
        // still preserves the variants (now under anyOf, not oneOf).
        let count = &edit_variants[0]["properties"]["count"];
        assert_eq!(count["nullable"], json!(true));
        let inner = count["anyOf"]
            .as_array()
            .expect("ReplaceCount variants survived");
        assert_eq!(inner.len(), 2);
        assert_eq!(inner[1]["enum"], json!(["all"]));
    }

    fn assert_no_forbidden_keys(value: &Value) {
        const FORBIDDEN: &[&str] = &[
            "$schema",
            "$defs",
            "definitions",
            "$id",
            "$ref",
            "$comment",
            "const",
            "oneOf",
            "additionalProperties",
            "unevaluatedProperties",
            "patternProperties",
        ];
        match value {
            Value::Object(map) => {
                for k in FORBIDDEN {
                    assert!(map.get(*k).is_none(), "found forbidden key `{k}` in {value}");
                }
                // Forbid `type: ["X", "null"]` arrays anywhere.
                if let Some(t) = map.get("type") {
                    assert!(!t.is_array(), "found array-typed `type` in {value}");
                }
                for v in map.values() {
                    assert_no_forbidden_keys(v);
                }
            }
            Value::Array(arr) => {
                for v in arr {
                    assert_no_forbidden_keys(v);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn passes_through_known_supported_keywords() {
        let input = json!({
            "type": "object",
            "properties": {
                "tags": { "type": "array", "items": { "type": "string" } },
                "kind": { "type": "string", "enum": ["a", "b"] }
            },
            "required": ["tags"]
        });
        let out = sanitize(&input);
        assert_eq!(out["properties"]["tags"]["items"]["type"], json!("string"));
        assert_eq!(out["properties"]["kind"]["enum"], json!(["a", "b"]));
        assert_eq!(out["required"], json!(["tags"]));
    }
}
