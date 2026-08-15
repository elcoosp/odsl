//! Identifier naming helpers used by the adapters.
//!
//! The canonical `snake_case` / `snake_plural` algorithms live in
//! [`osdl_core::naming`] so the migrations adapter agrees with the code
//! renderers on physical table/collection names. This module adds only the
//! backend-specific wrappers and SQL identifier quoting.

use osdl_core::to_snake_plural;

/// SQL table name: `snake_case` + plural (`users`, `blog_posts`).
pub fn table_name(model: &str) -> String {
    to_snake_plural(model)
}

/// MongoDB collection name: same `snake_case` + plural convention the Mongo
/// renderer emits (so migrations target the collections `osdl build` creates).
pub fn collection_name(model: &str) -> String {
    to_snake_plural(model)
}

/// Quote an identifier for the target SQL dialect (double quotes, ANSI).
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_snake_plural() {
        assert_eq!(table_name("User"), "users");
        assert_eq!(table_name("BlogPost"), "blog_posts");
        assert_eq!(table_name("Category"), "categories");
        assert_eq!(table_name("Box"), "boxes");
    }

    #[test]
    fn collections_match_renderer() {
        // Must equal the Mongo renderer's `to_snake_plural` output.
        assert_eq!(collection_name("User"), "users");
        assert_eq!(collection_name("BlogPost"), "blog_posts");
    }

    #[test]
    fn quoting_is_safe() {
        assert_eq!(quote_ident("users"), "\"users\"");
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
    }
}
