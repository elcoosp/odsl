//! The OSDL lockfile: a deterministic snapshot of a compiled schema.
//!
//! The lockfile stores the serializable projection of an [`Ast`] plus a SHA-256
//! checksum. Two structurally-equal schemas always produce byte-for-byte
//! identical lockfiles (REQ-NFR-DET-001), which is what makes auto-diffing
//! migrations reliable.

use crate::ast::{Ast, LockField, LockModel};
use crate::errors::OsdlError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The on-disk lockfile format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lockfile {
    /// Schema format version for forward compatibility.
    pub version: u32,
    /// SHA-256 of the canonical (sorted, JSON) projection below.
    pub checksum: String,
    /// The schema model projection (already deterministically sorted).
    pub models: Vec<LockModel>,
}

impl Lockfile {
    pub const VERSION: u32 = 1;

    /// Build a lockfile from a validated AST.
    pub fn from_ast(ast: &Ast) -> Self {
        let models = ast.to_lock();
        let checksum = compute_checksum(&models);
        Self {
            version: Self::VERSION,
            checksum,
            models,
        }
    }

    /// Serialize to a pretty, stable JSON string.
    pub fn to_string_pretty(&self) -> Result<String, OsdlError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Parse a lockfile from JSON text.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Result<Self, OsdlError> {
        Ok(serde_json::from_str(text)?)
    }

    /// The canonical checksum of the current projection (re-computed, ignoring
    /// the stored value) — used to decide whether a re-build changed anything.
    pub fn current_checksum(&self) -> String {
        compute_checksum(&self.models)
    }

    pub fn model_by_name(&self, name: &str) -> Option<&LockModel> {
        self.models.iter().find(|m| m.name == name)
    }
}

/// Deterministically hash the model projection. Sorting happens inside
/// [`Ast::to_lock`], so we simply hash the canonical JSON.
fn compute_checksum(models: &[LockModel]) -> String {
    let canonical = serde_json::to_string(models).expect("LockModel is serializable");
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2 + 7);
    hex.push_str("sha256:");
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// Convenience builder for a single [`LockField`], used in tests and the
/// migrator crate.
pub fn lock_field(name: &str, ty: &str, intents: &[&str]) -> LockField {
    LockField {
        name: name.to_string(),
        ty: ty.to_string(),
        intents: intents.iter().map(|s| s.to_string()).collect(),
        enum_variants: vec![],
        default_value: None,
        m2m_target: None,
        check_expr: None,
        polymorphic_targets: vec![],
        on_delete: None,
        on_update: None,
        numeric_precision: None,
        numeric_scale: None,
        through_model: None,
    }
}

/// Build a lock field that carries native-enum variants.
pub fn lock_enum_field(name: &str, ty: &str, intents: &[&str], variants: &[&str]) -> LockField {
    LockField {
        name: name.to_string(),
        ty: ty.to_string(),
        intents: intents.iter().map(|s| s.to_string()).collect(),
        enum_variants: variants.iter().map(|s| s.to_string()).collect(),
        default_value: None,
        m2m_target: None,
        check_expr: None,
        polymorphic_targets: vec![],
        on_delete: None,
        on_update: None,
        numeric_precision: None,
        numeric_scale: None,
        through_model: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Ast, Field, Model};
    use crate::types::{FieldType, Intent, ScalarType};
    use la_arena::Arena;

    fn sample_ast() -> Ast {
        let mut ast = Ast::new();
        let mut user = Model {
            name: "User".into(),
            fields: Arena::new(),
            field_index: vec![],
            line: 1,
            indexes: vec![],
            primary_key: vec![],
        };
        user.add_field(Field {
            custom_type: None,
            name: "id".into(),
            ty: FieldType::Scalar(ScalarType::Uuid),
            intents: vec![Intent::Pk],
            enum_variants: vec![],
            default_value: None,
            m2m_target: None,
            check_expr: None,
            polymorphic_targets: vec![],
            on_delete: None,
            on_update: None,
            numeric_precision: None,
            numeric_scale: None,
            through_model: None,
            line: 1,
        });
        user.add_field(Field {
            custom_type: None,
            name: "email".into(),
            ty: FieldType::Scalar(ScalarType::String),
            intents: vec![Intent::Uniq],
            enum_variants: vec![],
            default_value: None,
            m2m_target: None,
            check_expr: None,
            polymorphic_targets: vec![],
            on_delete: None,
            on_update: None,
            numeric_precision: None,
            numeric_scale: None,
            through_model: None,
            line: 2,
        });
        ast.add_model(user);
        ast
    }

    #[test]
    fn checksum_is_deterministic() {
        let a = Lockfile::from_ast(&sample_ast());
        let b = Lockfile::from_ast(&sample_ast());
        assert_eq!(a.checksum, b.checksum);
        assert!(a.checksum.starts_with("sha256:"));
    }

    #[test]
    fn round_trips_through_json() {
        let lf = Lockfile::from_ast(&sample_ast());
        let text = lf.to_string_pretty().unwrap();
        let parsed = Lockfile::from_str(&text).unwrap();
        assert_eq!(lf, parsed);
    }

    #[test]
    fn field_helper_builds() {
        let f = lock_field("name", "string", &["-uniq"]);
        assert_eq!(f.name, "name");
        assert_eq!(f.intents, vec!["-uniq".to_string()]);
    }

    #[test]
    fn enum_variants_survive_lockfile_round_trip() {
        use crate::ast::{Ast, Field, Model};
        let mut ast = Ast::new();
        let mut user = Model {
            name: "User".into(),
            fields: Arena::new(),
            field_index: vec![],
            line: 1,
            indexes: vec![],
            primary_key: vec![],
        };
        user.add_field(Field {
            custom_type: None,
            name: "status".into(),
            ty: FieldType::Scalar(ScalarType::String),
            intents: vec![Intent::Enum],
            enum_variants: vec!["active".into(), "inactive".into()],
            default_value: None,
            m2m_target: None,
            check_expr: None,
            polymorphic_targets: vec![],
            on_delete: None,
            on_update: None,
            numeric_precision: None,
            numeric_scale: None,
            through_model: None,
            line: 2,
        });
        ast.add_model(user);
        let lf = Lockfile::from_ast(&ast);
        let text = lf.to_string_pretty().unwrap();
        let parsed = Lockfile::from_str(&text).unwrap();
        assert_eq!(lf, parsed);
        let got = &parsed.models[0].fields[0].enum_variants;
        assert_eq!(got, &vec!["active".to_string(), "inactive".to_string()]);
    }

    #[test]
    fn model_indexes_survive_lockfile_round_trip() {
        use crate::ast::{Ast, Field, Model, ModelIndex};
        let mut ast = Ast::new();
        let mut user = Model {
            name: "User".into(),
            fields: Arena::new(),
            field_index: vec![],
            line: 1,
            indexes: vec![],
            primary_key: vec![],
        };
        user.add_field(Field {
            custom_type: None,
            name: "id".into(),
            ty: FieldType::Scalar(ScalarType::Uuid),
            intents: vec![Intent::Pk],
            enum_variants: vec![],
            default_value: None,
            m2m_target: None,
            check_expr: None,
            polymorphic_targets: vec![],
            on_delete: None,
            on_update: None,
            numeric_precision: None,
            numeric_scale: None,
            through_model: None,
            line: 2,
        });
        user.add_field(Field {
            custom_type: None,
            name: "tenant_id".into(),
            ty: FieldType::Scalar(ScalarType::Uuid),
            intents: vec![],
            enum_variants: vec![],
            default_value: None,
            m2m_target: None,
            check_expr: None,
            polymorphic_targets: vec![],
            on_delete: None,
            on_update: None,
            numeric_precision: None,
            numeric_scale: None,
            through_model: None,
            line: 3,
        });
        user.add_field(Field {
            custom_type: None,
            name: "email".into(),
            ty: FieldType::Scalar(ScalarType::String),
            intents: vec![],
            enum_variants: vec![],
            default_value: None,
            m2m_target: None,
            check_expr: None,
            polymorphic_targets: vec![],
            on_delete: None,
            on_update: None,
            numeric_precision: None,
            numeric_scale: None,
            through_model: None,
            line: 4,
        });
        user.indexes.push(ModelIndex {
            name: "uniq_tenant_id_email".into(),
            fields: vec!["tenant_id".into(), "email".into()],
            unique: true,
            index_type: None,
            prefix_length: None,
            where_clause: None,
            order: None,
            nulls: None,
        });
        ast.add_model(user);
        let lf = Lockfile::from_ast(&ast);
        let text = lf.to_string_pretty().unwrap();
        let parsed = Lockfile::from_str(&text).unwrap();
        assert_eq!(lf, parsed);
        let got = &parsed.models[0].indexes[0];
        assert_eq!(got.name, "uniq_tenant_id_email");
        assert_eq!(
            got.fields,
            vec!["tenant_id".to_string(), "email".to_string()]
        );
        assert!(got.unique);
    }

    #[test]
    fn m2m_expands_to_junction_table() {
        use crate::ast::{Ast, Field, Model};
        use crate::types::{Intent, ScalarType};
        let mut ast = Ast::new();
        let mut user = Model {
            name: "User".into(),
            fields: Arena::new(),
            field_index: vec![],
            line: 1,
            indexes: vec![],
            primary_key: vec![],
        };
        user.add_field(Field {
            custom_type: None,
            name: "id".into(),
            ty: FieldType::Scalar(ScalarType::Uuid),
            intents: vec![Intent::Pk],
            enum_variants: vec![],
            default_value: None,
            m2m_target: None,
            check_expr: None,
            polymorphic_targets: vec![],
            on_delete: None,
            on_update: None,
            numeric_precision: None,
            numeric_scale: None,
            through_model: None,
            line: 1,
        });
        user.add_field(Field {
            custom_type: None,
            name: "posts".into(),
            ty: FieldType::InferredRef("posts".into()),
            intents: vec![Intent::M2m],
            enum_variants: vec![],
            default_value: None,
            m2m_target: Some("Post".into()),
            check_expr: None,
            polymorphic_targets: vec![],
            on_delete: None,
            on_update: None,
            numeric_precision: None,
            numeric_scale: None,
            through_model: None,
            line: 2,
        });
        let mut post = Model {
            name: "Post".into(),
            fields: Arena::new(),
            field_index: vec![],
            line: 3,
            indexes: vec![],
            primary_key: vec![],
        };
        post.add_field(Field {
            custom_type: None,
            name: "id".into(),
            ty: FieldType::Scalar(ScalarType::Uuid),
            intents: vec![Intent::Pk],
            enum_variants: vec![],
            default_value: None,
            m2m_target: None,
            check_expr: None,
            polymorphic_targets: vec![],
            on_delete: None,
            on_update: None,
            numeric_precision: None,
            numeric_scale: None,
            through_model: None,
            line: 4,
        });
        ast.add_model(user);
        ast.add_model(post);
        let lf = Lockfile::from_ast(&ast);
        // The junction table exists and the m2m field is gone from User.
        let user_lm = lf.model_by_name("User").unwrap();
        assert!(user_lm.field_by_name("posts").is_none());
        let j = lf
            .model_by_name("User_Post")
            .expect("junction table missing");
        assert!(j.field_by_name("user_id").is_some());
        assert!(j.field_by_name("post_id").is_some());
        assert!(j.indexes.iter().any(|i| i.unique
            && i.fields == vec!["user_id".to_string(), "post_id".to_string()]));
    }
}
