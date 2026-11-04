//! Shared code-generation helpers for ODSL renderers.
//!
//! This crate centralizes the `syn` + `quote` + `prettyplease` machinery so the
//! SeaORM and MongoDB renderers stay DRY. The key guarantee (per the tech
//! stack): generated code is built from a real [`syn::File`] AST and formatted
//! with [`prettyplease`], so it is always syntactically valid — no stringly
//! concatenation, no macro syntax errors.

use std::path::Path;

/// Re-exported from [`odsl_core::naming`] (the single source of truth for
/// identifier naming) so existing `use odsl_codegen::to_pascal_case` callers
/// keep working.
pub use odsl_core::naming::to_pascal_case;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_case_idempotent() {
        assert_eq!(to_pascal_case("UserProfile"), "UserProfile");
        assert_eq!(to_pascal_case("user_profile"), "UserProfile");
        assert_eq!(to_pascal_case("created_at"), "CreatedAt");
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
        let dir = std::env::temp_dir().join("odsl_codegen_test/nested");
        let path = dir.join("out.rs");
        write_file(&path, "pub struct X;").unwrap();
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("odsl_codegen_test"));
    }
}
