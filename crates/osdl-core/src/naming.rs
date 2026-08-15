//! Shared identifier naming helpers.
//!
//! OSDL model names are `PascalCase`. Each backend needs a different physical
//! name, and — crucially — the migrations adapter must agree with the code
//! renderers on those names, or `osdl build` and `osdl migrate up` would
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
}
