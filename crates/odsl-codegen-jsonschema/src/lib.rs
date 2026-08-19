//! JSON Schema (draft 2020-12) renderer.
//!
//! Generates, per ODSL model, a standalone JSON Schema document. A combined
//! `schema.json` exposes every model under `$defs` and validates any one of
//! them via `oneOf`. Rendering is driven by the *expanded* lockfile projection
//! (`ast.to_lock()`), which materialises many-to-many relationships into
//! `<Source>_<Target>` junction models, so the generated schemas match what is
//! actually deployed. Doc comments (`///`) become `description`; `-deprecated`
//! becomes `deprecated: true`; `-check`/`-uniq`/`-default` are preserved as
//! `x-odsl-*` extension keywords.

#![allow(clippy::result_large_err)]

use odsl_core::Target;
use odsl_core::ast::{Ast, LockField, LockModel};
use odsl_core::errors::OdslError;
use odsl_core::validator::CodeRenderer;
use serde_json::{Value, json};

const DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";

/// The JSON Schema renderer.
pub struct JsonSchemaRenderer {
    target: Target,
}

impl JsonSchemaRenderer {
    pub fn new(target: Target) -> Self {
        Self { target }
    }
}

impl CodeRenderer for JsonSchemaRenderer {
    fn target(&self) -> Target {
        self.target
    }

    fn render(&self, ast: &Ast) -> Result<Vec<(String, String)>, OdslError> {
        let models = ast.to_lock();
        let mut files: Vec<(String, String)> = Vec::new();

        // Per-model files.
        for model in &models {
            let schema = build_model_schema(model, ast);
            let text = serde_json::to_string_pretty(&schema).map_err(|e| {
                OdslError::Io(std::io::Error::other(format!("serialize json-schema: {e}")))
            })?;
            files.push((format!("schema/{}.schema.json", model.name), text));
        }

        // Combined root with `$defs`.
        let root = build_combined(&models, ast);
        let root_text = serde_json::to_string_pretty(&root).map_err(|e| {
            OdslError::Io(std::io::Error::other(format!("serialize json-schema: {e}")))
        })?;
        files.push(("schema.json".to_string(), root_text));

        Ok(files)
    }
}

/// Map an ODSL type keyword (or `Model.field` reference) to a JSON Schema
/// fragment. References are stored as their (UUID/integer) id and annotated
/// with `x-odsl-ref` so the relationship survives.
fn json_type_for_ty(ty: &str) -> Value {
    if ty.contains('.') {
        return json!({ "type": "string", "x-odsl-ref": ty });
    }
    match ty {
        "string" => json!({ "type": "string" }),
        "int" => json!({ "type": "integer", "format": "int32" }),
        "bigint" => json!({ "type": "integer", "format": "int64" }),
        "float" => json!({ "type": "number", "format": "double" }),
        "bool" => json!({ "type": "boolean" }),
        "datetime" => json!({ "type": "string", "format": "date-time" }),
        "date" => json!({ "type": "string", "format": "date" }),
        "uuid" => json!({ "type": "string", "format": "uuid" }),
        "json" => json!({ "type": "object" }),
        "binary" => json!({ "type": "string", "format": "binary" }),
        other => json!({ "type": "string", "x-odsl-unknown-type": other }),
    }
}

fn has_intent(f: &LockField, kw: &str) -> bool {
    f.intents.iter().any(|i| i == kw)
}

/// Parse a `-default` literal into a JSON value where it is unambiguous
/// (numbers, booleans, `null`); otherwise keep it as a string.
fn parse_default(raw: &str) -> Value {
    match raw {
        "true" => json!(true),
        "false" => json!(false),
        "null" => json!(null),
        "now" => json!("now"),
        s if s.parse::<i64>().is_ok() => json!(s.parse::<i64>().unwrap()),
        s if s.parse::<f64>().is_ok() => json!(s.parse::<f64>().unwrap()),
        s => json!(s),
    }
}

/// Build a JSON Schema object for one model (or junction).
fn build_model_schema(model: &LockModel, ast: &Ast) -> Value {
    let mut props = serde_json::Map::new();
    let mut required: Vec<String> = Vec::new();

    for f in &model.fields {
        // Polymorphic references expand into a `<name>_type` discriminator and
        // a `<name>_id` reference, both required.
        if !f.polymorphic_targets.is_empty() {
            let base = to_snake(&f.name);
            props.insert(
                format!("{base}_type"),
                json!({ "type": "string", "x-odsl-polymorphic": f.polymorphic_targets }),
            );
            props.insert(
                format!("{base}_id"),
                json!({ "type": "string", "format": "uuid" }),
            );
            required.push(format!("{base}_type"));
            required.push(format!("{base}_id"));
            continue;
        }

        let mut schema = json_type_for_ty(&f.ty);

        if f.enum_variants.iter().any(|v| !v.is_empty()) {
            let mut variants: Vec<String> = f.enum_variants.clone();
            variants.retain(|v| !v.is_empty());
            variants.sort();
            schema["enum"] = json!(variants);
        }

        if has_intent(f, "-null")
            && let Some(obj) = schema.as_object_mut()
            && let Some(Value::String(t)) = obj.get("type")
        {
            let t = t.clone();
            obj.insert("type".into(), json!([t, "null"]));
        }

        if let Some(obj) = schema.as_object_mut() {
            if has_intent(f, "-uniq") {
                obj.insert("x-odsl-unique".into(), json!(true));
            }
            if let Some(expr) = &f.check_expr {
                obj.insert("x-odsl-check".into(), json!(expr));
            }
            if let Some(value) = &f.default_value {
                obj.insert("default".into(), parse_default(value));
            }
            // Doc comment -> description; deprecation -> deprecated: true.
            if let Some(doc) = ast.field_doc(&model.name, &f.name) {
                obj.insert("description".into(), json!(doc));
            }
            if ast.field_deprecation(&model.name, &f.name).is_some() {
                obj.insert("deprecated".into(), json!(true));
            }
        }

        props.insert(f.name.clone(), schema);
        if !has_intent(f, "-null") {
            required.push(f.name.clone());
        }
    }

    let mut schema = serde_json::Map::new();
    schema.insert("$schema".into(), json!(DRAFT));
    schema.insert("title".into(), json!(model.name));
    schema.insert("type".into(), json!("object"));
    schema.insert("additionalProperties".into(), json!(false));
    schema.insert("properties".into(), Value::Object(props));
    if !required.is_empty() {
        required.sort();
        required.dedup();
        schema.insert("required".into(), json!(required));
    }
    if let Some(doc) = ast.model_doc(&model.name) {
        schema.insert("description".into(), json!(doc));
    }
    Value::Object(schema)
}

/// Build the combined root document: every model under `$defs`, validated by
/// `oneOf` so the root accepts any single model instance.
fn build_combined(models: &[LockModel], ast: &Ast) -> Value {
    let mut defs = serde_json::Map::new();
    let mut one_of: Vec<Value> = Vec::new();
    let mut names: Vec<String> = models.iter().map(|m| m.name.clone()).collect();
    names.sort();
    names.dedup();
    for name in &names {
        let model = models
            .iter()
            .find(|m| &m.name == name)
            .expect("model exists");
        defs.insert(name.clone(), build_model_schema(model, ast));
        one_of.push(json!({ "$ref": format!("#/$defs/{name}") }));
    }
    json!({
        "$schema": DRAFT,
        "title": "ODSL-generated JSON Schemas",
        "$id": "https://odsl.dev/schema.json",
        "description": "Generated by `odsl build --target json-schema`. Do not edit by hand.",
        "$defs": Value::Object(defs),
        "oneOf": one_of,
    })
}

/// `ModelName` -> `model_name` (snake_case).
fn to_snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.char_indices() {
        if i != 0 && c.is_uppercase() {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use odsl_parser::parse;

    fn render(src: &str) -> Vec<(String, String)> {
        let ast = parse(src).expect("parse");
        odsl_core::Validator::validate(&ast, Some(Target::JsonSchema)).expect("validate");
        let r = JsonSchemaRenderer::new(Target::JsonSchema);
        r.render(&ast).expect("render")
    }

    #[test]
    fn emits_per_model_and_combined() {
        let files = render("User\n  id uuid -pk\n  email string -uniq\n  age int -null\n");
        let names: Vec<&String> = files.iter().map(|(n, _)| n).collect();
        assert!(
            names
                .iter()
                .any(|n| n.as_str() == "schema/User.schema.json")
        );
        assert!(names.iter().any(|n| n.as_str() == "schema.json"));
        // Combined root has $defs + oneOf.
        let (_, root) = files
            .iter()
            .find(|(n, _)| n.as_str() == "schema.json")
            .unwrap();
        let v: Value = serde_json::from_str(root).unwrap();
        assert_eq!(v["$schema"], json!(DRAFT));
        assert!(v["$defs"]["User"].is_object());
        assert!(v["oneOf"].as_array().unwrap().len() == 1);
    }

    #[test]
    fn maps_types_and_required_and_nullable() {
        let files = render("User\n  id uuid -pk\n  email string -uniq\n  age int -null\n");
        let (_, text) = files
            .iter()
            .find(|(n, _)| n == "schema/User.schema.json")
            .unwrap();
        let v: Value = serde_json::from_str(text).unwrap();
        assert_eq!(v["type"], json!("object"));
        let props = v["properties"].as_object().unwrap();
        // uuid -> string with format uuid.
        assert_eq!(props["id"]["type"], json!("string"));
        assert_eq!(props["id"]["format"], json!("uuid"));
        // int -> integer with format int32.
        assert_eq!(props["age"]["type"], json!(["integer", "null"]));
        assert_eq!(props["age"]["format"], json!("int32"));
        // required contains id + email (non-nullable), not age.
        let req = v["required"].as_array().unwrap();
        assert!(req.iter().any(|x| x == "id"));
        assert!(req.iter().any(|x| x == "email"));
        assert!(!req.iter().any(|x| x == "age"));
        // unique extension on email.
        assert_eq!(props["email"]["x-odsl-unique"], json!(true));
    }

    #[test]
    fn expands_m2m_junctions() {
        let files = render("User\n  id uuid -pk\n  groups -m2m Group\n\nGroup\n  id uuid -pk\n");
        let names: Vec<&String> = files.iter().map(|(n, _)| n).collect();
        assert!(
            names
                .iter()
                .any(|n| n.as_str() == "schema/User_Group.schema.json")
        );
        let (_, text) = files
            .iter()
            .find(|(n, _)| n.as_str() == "schema/User_Group.schema.json")
            .unwrap();
        let v: Value = serde_json::from_str(text).unwrap();
        let props = v["properties"].as_object().unwrap();
        // Junction carries the two FK columns.
        assert!(props.contains_key("user_id"));
        assert!(props.contains_key("group_id"));
        // FK references annotated.
        assert_eq!(props["user_id"]["x-odsl-ref"], json!("User.id"));
    }
}
