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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockField {
    pub name: String,
    pub ty: String,
    pub intents: Vec<String>,
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
                    .map(|(_, f)| LockField {
                        name: f.name.clone(),
                        ty: f.type_keyword(),
                        intents: f.intents.iter().map(|i| i.as_keyword().to_string()).collect(),
                    })
                    .collect();
                fields.sort_by(|a, b| a.name.cmp(&b.name));
                LockModel {
                    name: m.name.clone(),
                    fields,
                }
            })
            .collect();
        models.sort_by(|a, b| a.name.cmp(&b.name));
        models
    }
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
        };
        user.add_field(Field {
            name: "id".into(),
            ty: FieldType::Scalar(ScalarType::Uuid),
            intents: vec![Intent::Pk],
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
        };
        a.add_field(Field {
            name: "name".into(),
            ty: FieldType::Scalar(ScalarType::String),
            intents: vec![],
            line: 1,
        });
        let mut b = Model {
            name: "Apple".into(),
            fields: Arena::new(),
            field_index: vec![],
            line: 1,
        };
        b.add_field(Field {
            name: "id".into(),
            ty: FieldType::Scalar(ScalarType::Int),
            intents: vec![Intent::Pk],
            line: 1,
        });
        b.add_field(Field {
            name: "z".into(),
            ty: FieldType::Scalar(ScalarType::String),
            intents: vec![],
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
