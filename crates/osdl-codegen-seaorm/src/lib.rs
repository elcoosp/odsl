//! SeaORM 2.x renderer.
//!
//! Generates, per model, a SeaORM 2.0 entity module using the dense
//! model-first format: relations (`belongs_to` / `has_many`) live directly on
//! the `Model` struct. Output is built from a real [`syn::File`] so it is
//! always valid Rust.

#![allow(clippy::result_large_err)]

use osdl_codegen::format_tokens;
use osdl_core::Target;
use osdl_core::ast::{Ast, Field, Model};
use osdl_core::errors::OsdlError;
use osdl_core::types::{FieldType, Intent, Reference, ScalarType};
use osdl_core::validator::CodeRenderer;
use proc_macro2::TokenStream;
use quote::quote;

/// The SeaORM renderer (SQLite + Postgres share the same entity format).
pub struct SeaOrmRenderer {
    target: Target,
}

impl SeaOrmRenderer {
    pub fn new(target: Target) -> Self {
        Self { target }
    }
}

impl CodeRenderer for SeaOrmRenderer {
    fn target(&self) -> Target {
        self.target
    }

    fn render(&self, ast: &Ast) -> Result<Vec<(String, String)>, OsdlError> {
        let mut files = Vec::new();
        for (_idx, model) in ast.models() {
            let module_name = model.name.to_ascii_lowercase();
            let contents = render_model(model, self.target)?;
            files.push((format!("entity/{module_name}.rs"), contents));
        }
        // mod.rs exporting every model module.
        let mod_rs = render_mod_rs(ast);
        files.push(("entity/mod.rs".to_string(), mod_rs));
        Ok(files)
    }
}

fn render_model(model: &Model, target: Target) -> Result<String, OsdlError> {
    let table_name = to_snake_plural(&model.name);

    let mut field_defs: Vec<TokenStream> = Vec::new();
    for (_fidx, field) in model.fields() {
        field_defs.push(render_field(field, target));
    }

    let tokens = quote! {
        use sea_orm::entity::prelude::*;

        #[sea_orm::model]
        #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
        #[sea_orm(table_name = #table_name)]
        pub struct Model {
            #(#field_defs)*
        }

        impl ActiveModelBehavior for ActiveModel {}
    };

    Ok(format_tokens(tokens))
}

fn render_field(field: &Field, _target: Target) -> TokenStream {
    let name = syn::Ident::new(&field.name, proc_macro2::Span::call_site());

    // A `-relation Other` (has_many) field has no physical column: only emit the
    // relation field.
    if field.has(Intent::Relation)
        && let Some(target) = relation_target_entity(field)
    {
        let te = syn::Ident::new(&target.to_ascii_lowercase(), proc_macro2::Span::call_site());
        return quote! {
            #[sea_orm(has_many)]
            pub #name: HasMany<super::#te::Entity>,
        };
    }

    // A reference (`Other.field` / `Other.field -pk`) is a belongs_to: emit both
    // the foreign-key scalar column and the relation accessor field.
    if let Some(Reference {
        model: ref_m,
        field: ref_f,
    }) = field_reference(field)
    {
        let fk_col = name.clone();
        let fk_col_str = name.to_string();
        let ref_f_str = ref_f.clone();
        let fk_ty = rust_type_for_ref(field, &ref_f);
        let target_module =
            syn::Ident::new(&ref_m.to_ascii_lowercase(), proc_macro2::Span::call_site());
        let rel_name = syn::Ident::new(&ref_m.to_ascii_lowercase(), proc_macro2::Span::call_site());
        return quote! {
            pub #fk_col: #fk_ty,
            #[sea_orm(belongs_to, from = #fk_col_str, to = #ref_f_str)]
            pub #rel_name: HasOne<super::#target_module::Entity>,
        };
    }

    // Plain column.
    let ty = rust_type_for_field(field, _target);
    let mut attrs: Vec<TokenStream> = Vec::new();
    if field.has(Intent::Pk) {
        attrs.push(quote! { #[sea_orm(primary_key)] });
        if field.has(Intent::Auto) {
            if !matches!(&field.ty, FieldType::Scalar(ScalarType::Uuid)) {
                attrs.push(quote! { #[sea_orm(auto_increment = false)] });
            }
        } else {
            attrs.push(quote! { #[sea_orm(auto_increment = false)] });
        }
    }
    if field.has(Intent::Uniq) {
        attrs.push(quote! { #[sea_orm(unique)] });
    }

    quote! {
        #(#attrs)*
        pub #name: #ty,
    }
}

/// Rust type for a foreign-key scalar column; falls back to `Uuid` (the common
/// key type) when the field carries no explicit scalar type.
fn rust_type_for_ref(field: &Field, _ref_f: &str) -> TokenStream {
    if let FieldType::Scalar(s) = &field.ty {
        return scalar_rust_type(*s, field.has(Intent::Null));
    }
    scalar_rust_type(ScalarType::Uuid, field.has(Intent::Null))
}

/// Extract the target model name from a `-relation Model` field.
fn relation_target_entity(field: &Field) -> Option<String> {
    if let FieldType::InferredRef(s) = &field.ty
        && let Some(stripped) = s.strip_prefix("relation:")
    {
        return Some(stripped.to_string());
    }
    if let Some(Reference { model, .. }) = field_reference(field) {
        return Some(model);
    }
    None
}

/// Map a field to its SeaORM Rust type token.
fn rust_type_for_field(field: &Field, _target: Target) -> TokenStream {
    // Resolve reference -> foreign key type.
    if let Some(Reference {
        model: ref_m,
        field: ref_f,
    }) = field_reference(field)
    {
        // Foreign key type follows the referenced model's key type. We emit the
        // referenced key's Rust type; for simplicity we map common key types.
        // The actual join is expressed via the Relation enum.
        let _ = ref_f;
        // We don't know the referenced field's scalar without cross-lookup; use
        // a generic approach: keep the declared/inferred type if present.
        // If the field also declared an explicit scalar (e.g. user_id uuid), use it.
        if let FieldType::Scalar(s) = &field.ty {
            return scalar_rust_type(*s, field.has(Intent::Null));
        }
        // Fallback: reference with no explicit scalar -> assume Uuid key.
        let t = scalar_rust_type(ScalarType::Uuid, field.has(Intent::Null));
        let _ = ref_m;
        return t;
    }

    match &field.ty {
        FieldType::Scalar(s) => scalar_rust_type(*s, field.has(Intent::Null)),
        FieldType::InferredRef(s) => {
            // Inferred reference without explicit type: default to Uuid key.
            let _ = s;
            scalar_rust_type(ScalarType::Uuid, field.has(Intent::Null))
        }
        FieldType::Ref(r) => {
            let _ = &r.model;
            scalar_rust_type(ScalarType::Uuid, field.has(Intent::Null))
        }
    }
}

fn scalar_rust_type(s: ScalarType, nullable: bool) -> TokenStream {
    let base = match s {
        ScalarType::String => quote! { String },
        ScalarType::Int => quote! { i32 },
        ScalarType::BigInt => quote! { i64 },
        ScalarType::Float => quote! { f64 },
        ScalarType::Bool => quote! { bool },
        ScalarType::DateTime => quote! { chrono::DateTime<chrono::Utc> },
        ScalarType::Date => quote! { chrono::NaiveDate },
        ScalarType::Uuid => quote! { uuid::Uuid },
        ScalarType::Json => quote! { serde_json::Value },
        ScalarType::Binary => quote! { Vec<u8> },
    };
    if nullable {
        quote! { Option<#base> }
    } else {
        base
    }
}

fn field_reference(field: &Field) -> Option<Reference> {
    osdl_core::validator::field_reference(field)
}

/// `User` -> `users`, `Person` -> `people` (simple pluralization).
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
    use osdl_core::ast::Ast;
    use osdl_parser::parse;

    fn compile(src: &str) -> Ast {
        let ast = parse(src).unwrap();
        Validator::validate(&ast, Some(Target::SeaOrmSqlite)).unwrap();
        ast
    }

    #[test]
    fn renders_a_simple_model() {
        let ast = compile("User\n  id uuid -pk\n  email string -uniq\n");
        let renderer = SeaOrmRenderer::new(Target::SeaOrmSqlite);
        let files = renderer.render(&ast).unwrap();
        let (path, user_rs) = &files[0];
        assert_eq!(path, "entity/user.rs");
        assert!(user_rs.contains("DeriveEntityModel"));
        assert!(user_rs.contains("#[sea_orm::model]"));
        assert!(user_rs.contains("pub struct Model"));
        assert!(user_rs.contains("pub id: uuid::Uuid"));
        assert!(user_rs.contains("#[sea_orm(primary_key)]"));
        assert!(user_rs.contains("table_name = \"users\""));
    }

    #[test]
    fn renders_mod_rs_and_relations() {
        let ast = compile("User\n  id uuid -pk\nPost\n  id uuid -pk\n  author User.id\n");
        let renderer = SeaOrmRenderer::new(Target::SeaOrmSqlite);
        let files = renderer.render(&ast).unwrap();
        let mod_rs = files
            .iter()
            .find(|(p, _)| p == "entity/mod.rs")
            .unwrap()
            .1
            .clone();
        assert!(mod_rs.contains("pub mod user;"));
        assert!(mod_rs.contains("pub mod post;"));
        let post_rs = files
            .iter()
            .find(|(p, _)| p == "entity/post.rs")
            .unwrap()
            .1
            .clone();
        assert!(post_rs.contains("belongs_to"));
        assert!(post_rs.contains("HasOne<super::user::Entity>"));
        assert!(post_rs.contains("#[sea_orm::model]"));
    }

    #[test]
    fn snapshot_user_entity() {
        let ast = compile("User\n  id uuid -pk\n  email string -uniq\n  age int -null\n");
        let renderer = SeaOrmRenderer::new(Target::SeaOrmSqlite);
        let files = renderer.render(&ast).unwrap();
        let user_rs = files
            .iter()
            .find(|(p, _)| p == "entity/user.rs")
            .unwrap()
            .1
            .clone();
        insta::assert_snapshot!(user_rs);
    }
}
