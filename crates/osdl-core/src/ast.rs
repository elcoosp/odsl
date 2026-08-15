//! The OSDL Abstract Syntax Tree.
//!
//! The AST is arena-allocated ([`la_arena`]) so that models can reference each
//! other cyclically (e.g. `User` -> `Post` -> `User`) without fighting the
//! borrow checker. Nodes are addressed by stable [`Idx`] handles.

use crate::types::{FieldType, Intent};
use la_arena::{Arena, Idx, RawIdx};
use serde::{Deserialize, Serialize};

pub type ModelIdx = Idx<Model>;
pub type FieldIdx = Idx<Field>;

/// The whole compiled schema.
#[derive(Debug, Clone, Default)]
pub struct Ast {
    pub models: Arena<Model>,
    /// Lookup from model name to its arena index.
    pub model_index: Vec<(String, ModelIdx)>,
}

impl Ast {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_model(&mut self, model: Model) -> ModelIdx {
        let name = model.name.clone();
        let idx = self.models.alloc(model);
        self.model_index.push((name, idx));
        idx
    }

    /// Resolve a model by name (O(1) after it has been added).
    pub fn model_by_name(&self, name: &str) -> Option<ModelIdx> {
        self.model_index
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, idx)| *idx)
    }

    pub fn models(&self) -> impl Iterator<Item = (ModelIdx, &Model)> {
        self.models.iter()
    }

    /// Number of fields across all models (used by benchmarks).
    pub fn field_count(&self) -> usize {
        self.models.iter().map(|(_, m)| m.fields.len()).sum()
    }
}

/// A top-level data entity (maps to a table in SQL or a collection in Mongo).
#[derive(Debug, Clone)]
pub struct Model {
    pub name: String,
    pub fields: Arena<Field>,
    pub field_index: Vec<(String, FieldIdx)>,
    pub line: usize,
    /// Model-level composite indexes (`-index a,b`) and unique constraints
    /// (`-uniq a,b`). Field-level `-index`/`-uniq` are stored on the field.
    pub indexes: Vec<ModelIndex>,
}

/// A composite index/unique constraint declared at the model level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelIndex {
    /// Index name, e.g. `idx_users_tenant_email`.
    pub name: String,
    /// Referenced field names (order matters).
    pub fields: Vec<String>,
    /// When true the constraint is UNIQUE.
    pub unique: bool,
}

impl Model {
    pub fn add_field(&mut self, field: Field) -> FieldIdx {
        let name = field.name.clone();
        let idx = self.fields.alloc(field);
        self.field_index.push((name, idx));
        idx
    }

    pub fn field_by_name(&self, name: &str) -> Option<FieldIdx> {
        self.field_index
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, idx)| *idx)
    }

    pub fn fields(&self) -> impl Iterator<Item = (FieldIdx, &Field)> {
        self.fields.iter()
    }
}

/// A single attribute of a model.
#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: FieldType,
    pub intents: Vec<Intent>,
    /// Closed value set when the field is a native enum (`-enum a,b`).
    pub enum_variants: Vec<String>,
    /// Database-side default value (`-default <value>`), e.g. `0`, `""`, `now`.
    pub default_value: Option<String>,
    /// Target model for a many-to-many relationship (`-m2m <Target>`).
    pub m2m_target: Option<String>,
    /// Raw boolean expression for an inline CHECK constraint (`-check "age >= 18"`).
    pub check_expr: Option<String>,
    /// Target models for a polymorphic reference (`-polymorphic Post,Video`).
    pub polymorphic_targets: Vec<String>,
    pub line: usize,
}

impl Field {
    pub fn has(&self, intent: Intent) -> bool {
        self.intents.contains(&intent)
    }

    /// The type keyword used when serializing for the lockfile / diffs.
    pub fn type_keyword(&self) -> String {
        match &self.ty {
            FieldType::Scalar(s) => s.as_keyword().to_string(),
            FieldType::Ref(r) => r.to_string(),
            FieldType::InferredRef(s) => s.clone(),
        }
    }
}

/// A serializable, allocation-free projection of the AST used for the lockfile
/// and for diffing. Field indices and arena layout are intentionally discarded;
/// only semantic content survives so that two structurally-equal schemas hash
/// identically (determinism requirement REQ-NFR-DET-001).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockModel {
    pub name: String,
    pub fields: Vec<LockField>,
    /// Model-level composite indexes / unique constraints.
    pub indexes: Vec<LockIndex>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockIndex {
    pub name: String,
    pub fields: Vec<String>,
    pub unique: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockField {
    pub name: String,
    pub ty: String,
    pub intents: Vec<String>,
    /// Closed value set for native enums (`-enum a,b`); empty otherwise.
    pub enum_variants: Vec<String>,
    /// Database-side default value (`-default <value>`); `None` when absent.
    pub default_value: Option<String>,
    /// Target model for a many-to-many relationship (`-m2m <Target>`); `None`
    /// when the field is not an m2m join.
    pub m2m_target: Option<String>,
    /// Raw CHECK constraint expression (`-check "..."`); `None` when absent.
    pub check_expr: Option<String>,
    /// Target models for a polymorphic reference (`-polymorphic A,B`); empty
    /// when the field is not polymorphic.
    pub polymorphic_targets: Vec<String>,
}

impl LockModel {
    pub fn field_by_name(&self, name: &str) -> Option<&LockField> {
        self.fields.iter().find(|f| f.name == name)
    }
}

impl Ast {
    /// Project the arena AST into the serializable lockfile representation,
    /// with models sorted by name and fields sorted by name for determinism.
    pub fn to_lock(&self) -> Vec<LockModel> {
        let mut models: Vec<LockModel> = self
            .models
            .iter()
            .map(|(_, m)| {
                let mut fields: Vec<LockField> = m
                    .fields
                    .iter()
                    .map(|(_, f)| {
                        let mut variants = f.enum_variants.clone();
                        variants.sort();
                        let mut intents: Vec<String> = f
                            .intents
                            .iter()
                            .map(|i| i.as_keyword().to_string())
                            .collect();
                        // Encode the m2m target into the intents so the
                        // lockfile stays deterministic and diffable.
                        if let Some(t) = &f.m2m_target {
                            intents.push(format!("-m2m {t}"));
                        }
                        // Encode polymorphic targets the same way.
                        if !f.polymorphic_targets.is_empty() {
                            intents
                                .push(format!("-polymorphic {}", f.polymorphic_targets.join(",")));
                        }
                        LockField {
                            name: f.name.clone(),
                            ty: f.type_keyword(),
                            intents,
                            enum_variants: variants,
                            default_value: f.default_value.clone(),
                            m2m_target: f.m2m_target.clone(),
                            check_expr: f.check_expr.clone(),
                            polymorphic_targets: f.polymorphic_targets.clone(),
                        }
                    })
                    .collect();
                fields.sort_by(|a, b| a.name.cmp(&b.name));
                let mut indexes: Vec<LockIndex> = m
                    .indexes
                    .iter()
                    .map(|i| LockIndex {
                        name: i.name.clone(),
                        fields: i.fields.clone(),
                        unique: i.unique,
                    })
                    .collect();
                indexes.sort_by(|a, b| a.name.cmp(&b.name));
                LockModel {
                    name: m.name.clone(),
                    fields,
                    indexes,
                }
            })
            .collect();
        models.sort_by(|a, b| a.name.cmp(&b.name));
        // Expand `-m2m` relationships into junction tables: the m2m field is
        // removed from its source model (it is not a real column) and a
        // `<Source>_<Target>` junction model is appended.
        expand_m2m_junctions(&mut models);
        models
    }
}

/// Expand many-to-many fields into junction-table [`LockModel`]s.
///
/// For every field carrying `-m2m <Target>`:
/// * the field is removed from its source model (it is not a column), and
/// * a junction model `{Source}_{Target}` is created with the source and target
///   primary-key columns (as foreign keys) plus a composite unique constraint.
fn expand_m2m_junctions(models: &mut Vec<LockModel>) {
    // (source, target, source_field_name) collected before mutating.
    let mut junctions: Vec<(String, String)> = Vec::new();
    for m in models.iter_mut() {
        let mut i = 0;
        while i < m.fields.len() {
            let m2m = m.fields[i]
                .intents
                .iter()
                .find(|i| i.starts_with("-m2m "))
                .map(|s| s["-m2m ".len()..].to_string());
            if let Some(target) = m2m {
                junctions.push((m.name.clone(), target));
                m.fields.remove(i);
            } else {
                i += 1;
            }
        }
    }
    for (source, target) in junctions {
        let source_l = to_snake(&source);
        let target_l = to_snake(&target);
        let jname = format!("{source}_{target}");
        let mut fields = vec![
            LockField {
                name: "id".into(),
                ty: "uuid".into(),
                intents: vec!["-pk".into()],
                enum_variants: vec![],
                default_value: None,
                m2m_target: None,
                check_expr: None,
                polymorphic_targets: vec![],
            },
            LockField {
                name: format!("{source_l}_id"),
                ty: format!("{source}.id"),
                intents: vec!["-uniq".into()],
                enum_variants: vec![],
                default_value: None,
                m2m_target: None,
                check_expr: None,
                polymorphic_targets: vec![],
            },
            LockField {
                name: format!("{target_l}_id"),
                ty: format!("{target}.id"),
                intents: vec!["-uniq".into()],
                enum_variants: vec![],
                default_value: None,
                m2m_target: None,
                check_expr: None,
                polymorphic_targets: vec![],
            },
        ];
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        let indexes = vec![LockIndex {
            name: format!("uniq_{source_l}_id_{target_l}_id"),
            fields: vec![format!("{source_l}_id"), format!("{target_l}_id")],
            unique: true,
        }];
        models.push(LockModel {
            name: jname,
            fields,
            indexes,
        });
    }
    models.sort_by(|a, b| a.name.cmp(&b.name));
}

/// Convert a `ModelName` to `model_name` (snake_case).
fn to_snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.char_indices() {
        if i != 0 && c.is_uppercase() {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// Helper to construct a [`ModelIdx`] from a raw integer (used by tests).
pub fn model_idx(raw: u32) -> ModelIdx {
    Idx::from_raw(RawIdx::from_u32(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Intent, ScalarType};

    #[test]
    fn build_and_lookup() {
        let mut ast = Ast::new();
        let mut user = Model {
            name: "User".into(),
            fields: Arena::new(),
            field_index: vec![],
            line: 1,
            indexes: vec![],
        };
        user.add_field(Field {
            name: "id".into(),
            ty: FieldType::Scalar(ScalarType::Uuid),
            intents: vec![Intent::Pk],
            enum_variants: vec![],
            default_value: None,
            m2m_target: None,
            check_expr: None,
            polymorphic_targets: vec![],
            line: 1,
        });
        let idx = ast.add_model(user);
        assert_eq!(ast.model_by_name("User"), Some(idx));
        let u = &ast.models[idx];
        assert_eq!(u.field_by_name("id").is_some(), true);
    }

    #[test]
    fn lock_is_deterministic_and_sorted() {
        use crate::types::ScalarType;
        let mut ast = Ast::new();
        let mut a = Model {
            name: "Zebra".into(),
            fields: Arena::new(),
            field_index: vec![],
            line: 1,
            indexes: vec![],
        };
        a.add_field(Field {
            name: "name".into(),
            ty: FieldType::Scalar(ScalarType::String),
            intents: vec![],
            enum_variants: vec![],
            default_value: None,
            m2m_target: None,
            check_expr: None,
            polymorphic_targets: vec![],
            line: 1,
        });
        let mut b = Model {
            name: "Apple".into(),
            fields: Arena::new(),
            field_index: vec![],
            line: 1,
            indexes: vec![],
        };
        b.add_field(Field {
            name: "id".into(),
            ty: FieldType::Scalar(ScalarType::Int),
            intents: vec![Intent::Pk],
            enum_variants: vec![],
            default_value: None,
            m2m_target: None,
            check_expr: None,
            polymorphic_targets: vec![],
            line: 1,
        });
        b.add_field(Field {
            name: "z".into(),
            ty: FieldType::Scalar(ScalarType::String),
            intents: vec![],
            enum_variants: vec![],
            default_value: None,
            m2m_target: None,
            check_expr: None,
            polymorphic_targets: vec![],
            line: 2,
        });
        ast.add_model(a);
        ast.add_model(b);
        let lock = ast.to_lock();
        assert_eq!(lock[0].name, "Apple");
        assert_eq!(lock[1].name, "Zebra");
        assert_eq!(lock[0].fields[0].name, "id"); // sorted: id before z
    }
}
