//! Shared code-generation helpers for OSDL renderers.
//!
//! This crate centralizes the `syn` + `quote` + `prettyplease` machinery so the
//! SeaORM and MongoDB renderers stay DRY. The key guarantee (per the tech
//! stack): generated code is built from a real [`syn::File`] AST and formatted
//! with [`prettyplease`], so it is always syntactically valid — no stringly
//! concatenation, no macro syntax errors.

use std::path::Path;

/// Format a [`syn::File`] into a pretty, valid Rust source string.
pub fn format_file(file: &syn::File) -> String {
    prettyplease::unparse(file)
}

/// Parse a token stream (from `quote!`) into a [`syn::File`] and format it.
///
/// Panics only on a malformed token stream, which would indicate a bug in a
/// renderer, never in user input.
pub fn format_tokens(tokens: proc_macro2::TokenStream) -> String {
    let file: syn::File =
        syn::parse2(tokens).expect("renderer produced invalid Rust syntax; this is a compiler bug");
    format_file(&file)
}

/// Write `contents` to `path`, creating parent directories as needed.
pub fn write_file(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}

/// Turn a model/field name into a PascalCase identifier fragment.
///
/// OSDL model names are already PascalCase, but field names are `snake_case`;
/// this helper is idempotent for either and is used when building struct /
/// type identifiers.
pub fn to_pascal_case(s: &str) -> String {
    s.split(['_', '-', ' '])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_case_idempotent() {
        assert_eq!(to_pascal_case("UserProfile"), "UserProfile");
        assert_eq!(to_pascal_case("user_profile"), "UserProfile");
        assert_eq!(to_pascal_case_helper("created_at"), "CreatedAt");
    }

    #[test]
    fn formats_valid_rust() {
        let tokens = quote::quote! {
            pub struct Foo { pub id: i32 }
        };
        let out = format_tokens(tokens);
        assert!(out.contains("pub struct Foo"));
        assert!(out.contains("pub id: i32"));
    }

    #[test]
    fn writes_file_with_parents() {
        let dir = std::env::temp_dir().join("osdl_codegen_test/nested");
        let path = dir.join("out.rs");
        write_file(&path, "pub struct X;").unwrap();
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("osdl_codegen_test"));
    }

    fn to_pascal_case_helper(s: &str) -> String {
        to_pascal_case(s)
    }
}
