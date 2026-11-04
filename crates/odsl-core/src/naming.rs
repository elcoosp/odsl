//! Shared identifier naming helpers.
//!
//! ODSL model names are `PascalCase`. Each backend needs a different physical
//! name, and — crucially — the migrations adapter must agree with the code
//! renderers on those names, or `odsl build` and `odsl migrate up` would
//! target different tables/collections. Both renderers already use
//! [`to_snake_plural`], so it lives here as the single source of truth.

/// Convert `PascalCase` / `camelCase` to `snake_case`.
///
/// Every uppercase letter becomes `_lowercase` (matching the renderers, which
/// also prefix every capital with an underscore).
pub fn to_snake(name: &str) -> String {
    name.chars()
        .enumerate()
        .flat_map(|(i, c)| {
            if i != 0 && c.is_uppercase() {
                vec!['_', c.to_ascii_lowercase()]
            } else {
                vec![c.to_ascii_lowercase()]
            }
        })
        .collect()
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

/// `snake_case` + pluralized: `User` -> `users`, `BlogPost` -> `blog_posts`,
/// `Category` -> `categories`, `Box` -> `boxes`.
pub fn to_snake_plural(s: &str) -> String {
    let snake = to_snake(s);
    if snake.ends_with('y') && !ends_with_vowel_y(&snake) {
        format!("{}ies", &snake[..snake.len() - 1])
    } else if ends_with_s(&snake) {
        format!("{}es", snake)
    } else {
        format!("{}s", snake)
    }
}

/// Turn a `snake_case` / `PascalCase` name into a `PascalCase` identifier
/// fragment: `status` -> `Status`, `order_status` -> `OrderStatus`.
///
/// The operation is idempotent (`UserProfile` stays `UserProfile`). An empty
/// input yields `"X"` so the result is always a valid identifier when used to
/// build Rust/TS type names.
pub fn to_pascal_case(s: &str) -> String {
    let mut out: String = s
        .split(['_', '-', ' '])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect();
    if out.is_empty() {
        // Preserve the identifier guarantee for empty / separator-only input.
        out.push('X');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_cases() {
        assert_eq!(to_snake("User"), "user");
        assert_eq!(to_snake("BlogPost"), "blog_post");
        // Every capital becomes `_lower` (matches the renderers' algorithm).
        assert_eq!(to_snake("HTTPServer"), "h_t_t_p_server");
        assert_eq!(to_snake("User2FA"), "user2_f_a");
    }

    #[test]
    fn tables_are_snake_plural() {
        assert_eq!(to_snake_plural("User"), "users");
        assert_eq!(to_snake_plural("BlogPost"), "blog_posts");
        assert_eq!(to_snake_plural("Category"), "categories");
        assert_eq!(to_snake_plural("Box"), "boxes");
        assert_eq!(to_snake_plural("Bus"), "buses");
    }

    #[test]
    fn pascal_cases_and_is_idempotent() {
        assert_eq!(to_pascal_case("status"), "Status");
        assert_eq!(to_pascal_case("order_status"), "OrderStatus");
        assert_eq!(to_pascal_case("UserProfile"), "UserProfile");
        // Always yields a usable identifier.
        assert_eq!(to_pascal_case(""), "X");
    }
}
