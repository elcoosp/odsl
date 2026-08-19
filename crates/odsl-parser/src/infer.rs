//! Type inference rules (REQ-FUNC-002).
//!
//! When a field's type is omitted, we infer it from naming conventions and the
//! surrounding schema context:
//!
//! * `created_at` / `updated_at` / `timestamp` -> `datetime`
//! * `email` -> `string`
//! * `<model>_id` (e.g. `user_id`)            -> reference to that model's key
//! * `*_at` / `*_time`                         -> `datetime`
//! * otherwise                                -> `string` (the safe default)

use odsl_core::types::{FieldType, Reference, ScalarType};
use std::collections::HashSet;

/// Infer a [`FieldType`] for a field whose type token was omitted.
///
/// `known_models` lets us distinguish a foreign-key `user_id` from an ordinary
/// string. `model_name` is the enclosing model (used to avoid self-reference).
pub fn infer_field_type(
    field_name: &str,
    known_models: &HashSet<String>,
    model_name: &str,
) -> FieldType {
    let lower = field_name.to_ascii_lowercase();

    if lower == "email" || lower.ends_with("_email") {
        return FieldType::Scalar(ScalarType::String);
    }
    if lower == "created_at"
        || lower == "updated_at"
        || lower == "deleted_at"
        || lower.ends_with("_at")
        || lower.ends_with("_time")
        || lower == "timestamp"
    {
        return FieldType::Scalar(ScalarType::DateTime);
    }
    if lower == "created_date" || lower.ends_with("_date") || lower == "date" {
        return FieldType::Scalar(ScalarType::Date);
    }
    if let Some(target) = lower.strip_suffix("_id") {
        let cap = capitalize(target);
        if known_models.contains(&cap) && cap != model_name {
            return FieldType::Ref(Reference {
                model: cap,
                field: "id".into(),
            });
        }
    }
    FieldType::Scalar(ScalarType::String)
}

/// Capitalize the first character of `s` (used for model name matching).
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_datetimes_and_email() {
        let models: HashSet<String> = ["User", "Post"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            infer_field_type("created_at", &models, "User"),
            FieldType::Scalar(ScalarType::DateTime)
        );
        assert_eq!(
            infer_field_type("email", &models, "User"),
            FieldType::Scalar(ScalarType::String)
        );
        assert_eq!(
            infer_field_type("deleted_at", &models, "User"),
            FieldType::Scalar(ScalarType::DateTime)
        );
    }

    #[test]
    fn infers_foreign_key() {
        let models: HashSet<String> = ["User", "Post"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            infer_field_type("user_id", &models, "Post"),
            FieldType::Ref(Reference {
                model: "User".into(),
                field: "id".into()
            })
        );
        // self-reference stays a plain string (no User model to point at)
        assert_eq!(
            infer_field_type("user_id", &models, "User"),
            FieldType::Scalar(ScalarType::String)
        );
    }

    #[test]
    fn unknown_field_defaults_to_string() {
        let models = HashSet::new();
        assert_eq!(
            infer_field_type("nickname", &models, "User"),
            FieldType::Scalar(ScalarType::String)
        );
    }
}
