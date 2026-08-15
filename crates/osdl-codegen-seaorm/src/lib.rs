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
    let mut enum_defs: Vec<TokenStream> = Vec::new();
    for (_fidx, field) in model.fields() {
        field_defs.push(render_field(field, target));
        if field.has(Intent::Enum) && !field.enum_variants.is_empty() {
            enum_defs.push(render_active_enum(field));
        }
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

        #(#enum_defs)*
    };

    // Model-level composite indexes / unique constraints.
    let index_tokens = render_indexes(model);
    let tokens = if let Some((attr, defs)) = index_tokens {
        quote! {
            #attr
            #tokens
            #defs
        }
    } else {
        tokens
    };

    Ok(format_tokens(tokens))
}

/// Emit SeaORM index support for model-level `-index`/`-uniq` constraints.
///
/// Returns `(attribute, Index_impl)` where `attribute` is the
/// `#[sea_orm(indexes(<Name>))]` placed on the `Model` struct and `Index_impl`
/// is the `Index` struct defining the comprised columns. When the model has no
/// indexes, returns `None` and no extra code is generated.
fn render_indexes(model: &Model) -> Option<(TokenStream, TokenStream)> {
    if model.indexes.is_empty() {
        return None;
    }
    let names: Vec<TokenStream> = model
        .indexes
        .iter()
        .map(|idx| {
            let n = syn::Ident::new(&to_pascal_case(&idx.name), proc_macro2::Span::call_site());
            quote! { #n }
        })
        .collect();
    let attr = quote! { #[sea_orm(indexes(#(#names),*))] };
    let defs: Vec<TokenStream> = model
        .indexes
        .iter()
        .map(|idx| {
            let name_ident =
                syn::Ident::new(&to_pascal_case(&idx.name), proc_macro2::Span::call_site());
            let name_str = idx.name.clone();
            let unique = idx.unique;
            let col_strs: Vec<TokenStream> = idx
                .fields
                .iter()
                .map(|f| {
                    let s = f.clone();
                    quote! { #s }
                })
                .collect();
            quote! {
                #[derive(Copy, Clone, Debug, PartialEq, Eq)]
                pub struct #name_ident;

                impl sea_orm::entity::IndexName for #name_ident {
                    fn get_index_name(&self) -> &str {
                        #name_str
                    }
                }

                impl sea_orm::entity::Index for #name_ident {
                    fn name(&self) -> Option<&str> {
                        Some(#name_str)
                    }
                    fn unique(&self) -> bool {
                        #unique
                    }
                    fn columns(&self) -> Vec<&str> {
                        vec![#(#col_strs),*]
                    }
                    fn is_composite(&self) -> bool {
                        true
                    }
                }
            }
        })
        .collect();
    Some((attr, quote! { #(#defs)* }))
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

    // Native enum: emit an ActiveEnum-backed column.
    if field.has(Intent::Enum) && !field.enum_variants.is_empty() {
        let enum_ident =
            syn::Ident::new(&to_pascal_case(&field.name), proc_macro2::Span::call_site());
        let col_ty = enum_ident.clone();
        let default_attr = default_attr(field);
        return quote! {
            #(#attrs)*
            #[sea_orm(active_enum = #enum_ident)]
            #default_attr
            pub #name: #col_ty,
        };
    }

    let default_attr = default_attr(field);
    quote! {
        #(#attrs)*
        #default_attr
        pub #name: #ty,
    }
}

/// Build the SeaORM `default_value` attribute for a field, if any.
/// `now` on temporal columns maps to the portable `CURRENT_TIMESTAMP`.
fn default_attr(field: &Field) -> TokenStream {
    let Some(value) = &field.default_value else {
        return quote! {};
    };
    let db_value = if value == "now"
        && matches!(
            field.ty,
            FieldType::Scalar(ScalarType::DateTime) | FieldType::Scalar(ScalarType::Date)
        ) {
        "CURRENT_TIMESTAMP".to_string()
    } else {
        value.clone()
    };
    quote! { #[sea_orm(default_value = #db_value)] }
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

/// Generate a SeaORM `ActiveEnum` struct for a `-enum` field.
fn render_active_enum(field: &Field) -> TokenStream {
    let enum_ident = syn::Ident::new(&to_pascal_case(&field.name), proc_macro2::Span::call_site());
    let variants: Vec<TokenStream> = field
        .enum_variants
        .iter()
        .map(|v| {
            let v_ident = syn::Ident::new(&to_pascal_case(v), proc_macro2::Span::call_site());
            quote! { #v_ident }
        })
        .collect();
    quote! {
        #[derive(Debug, Clone, PartialEq, Eq, EnumIter, ActiveEnum)]
        #[sea_orm(rs_type = "String", db_type = "Text", rename_all = "snake_case")]
        pub enum #enum_ident {
            #(#variants),*
        }
    }
}

/// `status` -> `Status`, `order_status` -> `OrderStatus`.
fn to_pascal_case(s: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for c in s.chars() {
        if c == '_' || c == '-' || c == ' ' {
            upper = true;
            continue;
        }
        if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    if out.is_empty() {
        out.push('X');
    }
    out
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
    fn renders_active_enum_field() {
        let ast = compile("User\n  id uuid -pk\n  status string -enum active,inactive,pending\n");
        let renderer = SeaOrmRenderer::new(Target::SeaOrmSqlite);
        let files = renderer.render(&ast).unwrap();
        let user_rs = files
            .iter()
            .find(|(p, _)| p == "entity/user.rs")
            .unwrap()
            .1
            .clone();
        assert!(
            user_rs.contains("active_enum = Status"),
            "enum column attr missing:\n{user_rs}"
        );
        assert!(
            user_rs.contains("pub status: Status"),
            "enum column type missing"
        );
        assert!(user_rs.contains("#[derive(Debug, Clone, PartialEq, Eq, EnumIter, ActiveEnum)]"));
        assert!(user_rs.contains("pub enum Status {"));
        assert!(user_rs.contains("Active"));
        assert!(user_rs.contains("Inactive"));
        assert!(user_rs.contains("Pending"));
    }

    #[test]
    fn renders_entity_level_composite_index() {
        let ast = compile(
            "User\n  id uuid -pk\n  tenant_id uuid\n  email string\n  -uniq tenant_id,email\n",
        );
        let renderer = SeaOrmRenderer::new(Target::SeaOrmSqlite);
        let files = renderer.render(&ast).unwrap();
        let user_rs = files
            .iter()
            .find(|(p, _)| p == "entity/user.rs")
            .unwrap()
            .1
            .clone();
        // Entity-level index attribute referencing the generated index struct.
        assert!(
            user_rs.contains("#[sea_orm(indexes(UniqTenantIdEmail))]"),
            "indexes attribute missing:\\n{user_rs}"
        );
        // The generated index struct implements IndexName + Index.
        assert!(user_rs.contains("impl sea_orm::entity::IndexName for UniqTenantIdEmail"));
        assert!(user_rs.contains("impl sea_orm::entity::Index for UniqTenantIdEmail"));
        assert!(user_rs.contains("fn unique(&self) -> bool {"));
        assert!(user_rs.contains("\"tenant_id\""));
        assert!(user_rs.contains("\"email\""));
    }

    #[test]
    fn renders_default_value_attr() {
        let ast =
            compile("User\n  id uuid -pk\n  age int -default 0\n  created datetime -default now\n");
        let renderer = SeaOrmRenderer::new(Target::SeaOrmSqlite);
        let files = renderer.render(&ast).unwrap();
        let user_rs = files
            .iter()
            .find(|(p, _)| p == "entity/user.rs")
            .unwrap()
            .1
            .clone();
        assert!(
            user_rs.contains("default_value = \"0\""),
            "int default missing:\\n{user_rs}"
        );
        assert!(
            user_rs.contains("default_value = \"CURRENT_TIMESTAMP\""),
            "now default should map to CURRENT_TIMESTAMP:\\n{user_rs}"
        );
    }
}
