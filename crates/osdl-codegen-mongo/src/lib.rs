//! MongoDB 3.x renderer.
//!
//! Generates, per model, a Serde struct backed by `bson`/`serde` types and a
//! `$jsonSchema` validator document (used by the migrator to apply
//! collection validators). Like the SeaORM renderer, all Rust output is built
//! from a real [`syn::File`] so it is always valid.

#![allow(clippy::result_large_err)]

use osdl_codegen::format_tokens;
use osdl_core::Target;
use osdl_core::ast::{Ast, Field, Model};
use osdl_core::errors::OsdlError;
use osdl_core::types::{FieldType, Intent, ScalarType};
use osdl_core::validator::CodeRenderer;
use proc_macro2::TokenStream;
use quote::quote;
use std::collections::BTreeMap;

/// The MongoDB renderer.
pub struct MongoRenderer {
    target: Target,
}

impl MongoRenderer {
    pub fn new(target: Target) -> Self {
        Self { target }
    }
}

impl CodeRenderer for MongoRenderer {
    fn target(&self) -> Target {
        self.target
    }

    fn render(&self, ast: &Ast) -> Result<Vec<(String, String)>, OsdlError> {
        let mut files = Vec::new();
        for (_idx, model) in ast.models() {
            let module_name = model.name.to_ascii_lowercase();
            let rust = render_struct(model)?;
            files.push((format!("entity/{module_name}.rs"), rust));
            // jsonSchema validator as a standalone JSON file.
            let schema = render_json_schema(model);
            files.push((format!("entity/{module_name}.json"), schema));
        }
        let mod_rs = render_mod_rs(ast);
        files.push(("entity/mod.rs".to_string(), mod_rs));
        Ok(files)
    }
}

fn render_struct(model: &Model) -> Result<String, OsdlError> {
    let struct_ident = syn::Ident::new(&model.name, proc_macro2::Span::call_site());
    let collection = to_snake_plural(&model.name);

    let mut fields: Vec<TokenStream> = Vec::new();
    for (_fidx, field) in model.fields() {
        let name = syn::Ident::new(&field.name, proc_macro2::Span::call_site());
        let ty = rust_type_for_field(field);
        let serde_name = &field.name;
        let mut attrs: Vec<TokenStream> = vec![quote! { #[serde(rename = #serde_name)] }];
        if field.has(Intent::Null) {
            attrs.push(quote! { #[serde(skip_serializing_if = "Option::is_none")] });
        }
        fields.push(quote! {
            #(#attrs)*
            pub #name: #ty,
        });
    }

    // Use `bson::oid::ObjectId` as the Mongo `_id` for the partition/pk.
    let tokens = quote! {
        use serde::{Deserialize, Serialize};
        use bson::oid::ObjectId;

        /// Generated from OSDL model `#struct_ident`.
        #[derive(Debug, Clone, Serialize, Deserialize)]
        pub struct #struct_ident {
            #[serde(rename = "_id", skip_serializing_if = "Option::is_none")]
            pub id: Option<ObjectId>,
            #(#fields)*
        }

        impl #struct_ident {
            /// The MongoDB collection name for this model.
            pub const COLLECTION: &'static str = #collection;
        }
    };
    Ok(format_tokens(tokens))
}

fn rust_type_for_field(field: &Field) -> TokenStream {
    let base = match &field.ty {
        FieldType::Scalar(s) => scalar_rust_type(*s),
        FieldType::Ref(r) => {
            // Foreign key stored as an ObjectId reference.
            let _ = &r.model;
            quote! { bson::oid::ObjectId }
        }
        FieldType::InferredRef(s) => {
            let _ = s;
            quote! { bson::oid::ObjectId }
        }
    };
    // Native enums serialize as plain strings in Mongo.
    if field.has(Intent::Enum) {
        return quote! { String };
    }
    if field.has(Intent::Null) {
        quote! { Option<#base> }
    } else {
        base
    }
}

fn scalar_rust_type(s: ScalarType) -> TokenStream {
    match s {
        ScalarType::String => quote! { String },
        ScalarType::Int => quote! { i32 },
        ScalarType::BigInt => quote! { i64 },
        ScalarType::Float => quote! { f64 },
        ScalarType::Bool => quote! { bool },
        ScalarType::DateTime => quote! { bson::DateTime },
        ScalarType::Date => quote! { bson::DateTime },
        ScalarType::Uuid => quote! { bson::Uuid },
        ScalarType::Json => quote! { bson::Document },
        ScalarType::Binary => quote! { bson::Binary },
    }
}

/// Build a MongoDB `$jsonSchema` validator document for the model.
fn render_json_schema(model: &Model) -> String {
    let mut props: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for (_fidx, field) in model.fields() {
        let mut prop = serde_json::json!({
            "bsonType": bson_type_for(field),
        });
        if field.has(Intent::Uniq) {
            prop["unique"] = serde_json::json!(true);
        }
        if field.has(Intent::Null) {
            // nullable: represented as an array of allowed types
            prop["bsonType"] = serde_json::json!([bson_type_for(field), "null"]);
        }
        if field.has(Intent::Fulltext) {
            prop["osdlIndex"] = serde_json::json!("text");
        }
        if field.has(Intent::Enum) && !field.enum_variants.is_empty() {
            prop["enum"] = serde_json::json!(field.enum_variants);
        }
        if let Some(value) = &field.default_value {
            // `now` is documented as a server-side default; record it as a hint.
            prop["default"] = serde_json::json!(value);
        }
        props.insert(field.name.clone(), prop);
    }
    // Required = non-nullable fields.
    let required: Vec<String> = model
        .fields()
        .filter(|(_, f)| !f.has(Intent::Null))
        .map(|(_, f)| f.name.clone())
        .collect();

    let schema = serde_json::json!({
        "$jsonSchema": {
            "bsonType": "object",
            "title": model.name,
            "required": required,
            "properties": props,
        }
    });
    serde_json::to_string_pretty(&schema).unwrap()
}

fn bson_type_for(field: &Field) -> String {
    match &field.ty {
        FieldType::Scalar(s) => match s {
            ScalarType::String => "string",
            ScalarType::Int => "int",
            ScalarType::BigInt => "long",
            ScalarType::Float => "double",
            ScalarType::Bool => "bool",
            ScalarType::DateTime | ScalarType::Date => "date",
            ScalarType::Uuid => "uuid",
            ScalarType::Json => "object",
            ScalarType::Binary => "binData",
        }
        .to_string(),
        FieldType::Ref(_) | FieldType::InferredRef(_) => "objectId".to_string(),
    }
}

fn to_snake_plural(s: &str) -> String {
    let snake = s
        .chars()
        .enumerate()
        .flat_map(|(i, c)| {
            if i != 0 && c.is_uppercase() {
                vec!['_', c.to_ascii_lowercase()]
            } else {
                vec![c.to_ascii_lowercase()]
            }
        })
        .collect::<String>();
    if snake.ends_with('y') && !ends_with_vowel_y(&snake) {
        format!("{}ies", &snake[..snake.len() - 1])
    } else if ends_with_s(&snake) {
        format!("{}es", snake)
    } else {
        format!("{}s", snake)
    }
}

fn ends_with_vowel_y(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 2 {
        return false;
    }
    matches!(bytes[bytes.len() - 2], b'a' | b'e' | b'i' | b'o' | b'u')
}

fn ends_with_s(s: &str) -> bool {
    s.ends_with('s')
        || s.ends_with("ch")
        || s.ends_with("sh")
        || s.ends_with("x")
        || s.ends_with('z')
}

fn render_mod_rs(ast: &Ast) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (_idx, model) in ast.models() {
        let module = model.name.to_ascii_lowercase();
        lines.push(format!("pub mod {module};"));
    }
    lines.sort();
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use osdl_core::Validator;
    use osdl_parser::parse;

    fn compile(src: &str) -> Ast {
        let ast = parse(src).unwrap();
        Validator::validate(&ast, Some(Target::Mongo)).unwrap();
        ast
    }

    #[test]
    fn renders_mongo_struct() {
        let ast = compile("User\n  id uuid -pk\n  email string -uniq\n  age int -null\n");
        let renderer = MongoRenderer::new(Target::Mongo);
        let files = renderer.render(&ast).unwrap();
        let user_rs = files
            .iter()
            .find(|(p, _)| p == "entity/user.rs")
            .unwrap()
            .1
            .clone();
        assert!(user_rs.contains("pub struct User"));
        assert!(user_rs.contains("use serde::{Deserialize, Serialize};"));
        assert!(user_rs.contains("pub email: String"));
        assert!(user_rs.contains("pub age: Option<i32>"));
        assert!(user_rs.contains("pub id: Option<ObjectId>"));
    }

    #[test]
    fn renders_default_value_in_schema() {
        let ast = compile("User\n  id uuid -pk\n  age int -default 0\n");
        let renderer = MongoRenderer::new(Target::Mongo);
        let files = renderer.render(&ast).unwrap();
        let schema = files
            .iter()
            .find(|(p, _)| p == "entity/user.json")
            .unwrap()
            .1
            .clone();
        let v: serde_json::Value = serde_json::from_str(&schema).unwrap();
        assert_eq!(v["$jsonSchema"]["properties"]["age"]["default"], "0");
    }

    #[test]
    fn renders_enum_as_string_with_constraint() {
        let ast = compile("User\n  id uuid -pk\n  status string -enum active,inactive\n");
        let renderer = MongoRenderer::new(Target::Mongo);
        let files = renderer.render(&ast).unwrap();
        let user_rs = files
            .iter()
            .find(|(p, _)| p == "entity/user.rs")
            .unwrap()
            .1
            .clone();
        assert!(
            user_rs.contains("pub status: String"),
            "enum field should be String:\n{user_rs}"
        );
        let schema = files
            .iter()
            .find(|(p, _)| p == "entity/user.json")
            .unwrap()
            .1
            .clone();
        let v: serde_json::Value = serde_json::from_str(&schema).unwrap();
        let prop = &v["$jsonSchema"]["properties"]["status"];
        assert_eq!(prop["bsonType"], "string");
        assert_eq!(prop["enum"], serde_json::json!(["active", "inactive"]));
    }
}
