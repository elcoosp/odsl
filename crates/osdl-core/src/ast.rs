//! The OSDL Abstract Syntax Tree.
//!
//! The AST is arena-allocated ([`la_arena`]) so that models can reference each
//! other cyclically (e.g. `User` -> `Post` -> `User`) without fighting the
//! borrow checker. Nodes are addressed by stable [`Idx`] handles.

use crate::types::{FieldType, FkAction, Intent, ScalarType};
use la_arena::{Arena, Idx, RawIdx};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type ModelIdx = Idx<Model>;
pub type FieldIdx = Idx<Field>;

/// A user-defined value object: `type Email = string -check "..."`.
/// Expands inline to its base scalar plus inherited intents/check.
#[derive(Debug, Clone)]
pub struct CustomType {
    pub name: String,
    pub base: ScalarType,
    pub intents: Vec<Intent>,
    pub enum_variants: Vec<String>,
    pub default_value: Option<String>,
    pub check_expr: Option<String>,
}

/// Schema-level configuration declared in a top-level `config` block.
///
/// ```text
/// config
///   default-type uuid
///   timestamp-format iso8601
///   soft-delete field=deleted_at
///   audit created_at,updated_at
/// ```
///
/// `default-type` sets the implicit key type used when a field's type is
/// omitted; `timestamp-format` records the canonical timestamp representation;
/// `soft-delete` names the column that marks logical deletion; `audit` lists
/// the created/updated tracking columns. The linter consults these so it no
/// longer flags every model for the schema-wide convention.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaConfig {
    /// Implicit key type for fields declared without an explicit type, e.g. `uuid`.
    pub default_type: Option<String>,
    /// Canonical timestamp representation, e.g. `iso8601`.
    pub timestamp_format: Option<String>,
    /// Column that marks logical (soft) deletion, e.g. `deleted_at`.
    pub soft_delete_field: Option<String>,
    /// Created/updated audit tracking columns, e.g. `[created_at, updated_at]`.
    pub audit_fields: Vec<String>,
}

/// The whole compiled schema.
#[derive(Debug, Clone, Default)]
pub struct Ast {
    pub models: Arena<Model>,
    /// Lookup from model name to its arena index.
    pub model_index: Vec<(String, ModelIdx)>,
    /// User-defined types (`type X = ...`), keyed by name.
    pub custom_types: Vec<(String, CustomType)>,
    /// Doc comments (`///`) attached to models, keyed by model name. Multiple
    /// consecutive `///` lines are joined with `\n`. Purely documentation —
    /// excluded from structural/determinism hashing.
    pub model_docs: HashMap<String, String>,
    /// Doc comments (`///`) attached to fields, keyed by `(model, field)`.
    pub field_docs: HashMap<(String, String), String>,
    /// `-deprecated "reason"` annotations on fields, keyed by `(model, field)`.
    /// The value is the human-readable deprecation reason.
    pub field_deprecated: HashMap<(String, String), String>,
    /// Schema-level configuration from the top-level `config` block, if any.
    pub config: SchemaConfig,
    /// Read-models / projections declared with top-level `view` blocks. Kept as
    /// a *separate* collection from `models` so the two namespaces never
    /// collide and no model/field/lockfile literal has to change when views are
    /// added.
    pub views: Vec<View>,
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

    /// Add a view (read-model) to the schema.
    pub fn add_view(&mut self, view: View) {
        self.views.push(view);
    }

    /// Resolve a view by name.
    pub fn view_by_name(&self, name: &str) -> Option<&View> {
        self.views.iter().find(|v| v.name == name)
    }

    /// Iterate the views in declaration order.
    pub fn views(&self) -> impl Iterator<Item = &View> {
        self.views.iter()
    }

    /// Resolve a custom type by name.
    pub fn custom_type_by_name(&self, name: &str) -> Option<&CustomType> {
        self.custom_types
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, ct)| ct)
    }

    /// Register a custom type (used during parsing).
    pub fn add_custom_type(&mut self, ct: CustomType) {
        let name = ct.name.clone();
        self.custom_types.push((name, ct));
    }

    /// Doc comment attached to a model (`///`), if any.
    pub fn model_doc(&self, model: &str) -> Option<&str> {
        self.model_docs.get(model).map(|s| s.as_str())
    }

    /// Doc comment attached to a field (`///`), if any.
    pub fn field_doc(&self, model: &str, field: &str) -> Option<&str> {
        self.field_docs
            .get(&(model.to_string(), field.to_string()))
            .map(|s| s.as_str())
    }

    /// Deprecation reason for a field (`-deprecated "..."`), if any.
    pub fn field_deprecation(&self, model: &str, field: &str) -> Option<&str> {
        self.field_deprecated
            .get(&(model.to_string(), field.to_string()))
            .map(|s| s.as_str())
    }

    /// Tolerant structural comparison used by `osdl migrate test` to assert a
    /// live database matches a target schema.
    ///
    /// Two schemas "match" when every model in `expected` exists in `actual`
    /// with the same set of fields, and each field agrees on its primary-key,
    /// unique, and nullable flags. **Type strings are intentionally ignored**
    /// because database round-trips normalize them (e.g. OSDL `int` is stored
    /// as `INTEGER` and introspected back as `int` only after a best-effort
    /// map; `uuid` becomes a `TEXT`/`UUID` column). Documenting directives
    /// (`///`, `-deprecated`) are also ignored — they are not physical schema.
    ///
    /// Returns `Ok(())` when the schemas match, or an `Err` describing the
    /// first mismatch. `actual` may legitimately contain extra models/fields
    /// (e.g. an `_osdl_migrations` tracker); only `expected`'s requirements are
    /// checked, so callers should diff against the *target* as `expected`.
    pub fn schema_matches(&self, actual: &Ast) -> Result<(), String> {
        // Build a name->model lookup for the actual schema.
        let actual_models: std::collections::HashMap<String, &Model> =
            actual.models().map(|(_, m)| (m.name.clone(), m)).collect();

        for (_, expected_model) in self.models() {
            let actual_model = actual_models.get(&expected_model.name).ok_or_else(|| {
                format!("missing model `{}` in live database", expected_model.name)
            })?;

            // Field lookup for the actual model.
            let actual_fields: std::collections::HashMap<String, &Field> = actual_model
                .fields()
                .map(|(_, f)| (f.name.clone(), f))
                .collect();

            for (_, ef) in expected_model.fields() {
                let af = actual_fields.get(&ef.name).ok_or_else(|| {
                    format!(
                        "missing field `{}.{}` in live database",
                        expected_model.name, ef.name
                    )
                })?;

                let e_pk = ef.has(crate::types::Intent::Pk);
                let a_pk = af.has(crate::types::Intent::Pk);
                if e_pk != a_pk {
                    return Err(format!(
                        "field `{}.{}` primary-key mismatch (expected {}, got {})",
                        expected_model.name, ef.name, e_pk, a_pk
                    ));
                }

                let e_uniq = ef.has(crate::types::Intent::Uniq);
                let a_uniq = af.has(crate::types::Intent::Uniq);
                if e_uniq != a_uniq {
                    return Err(format!(
                        "field `{}.{}` unique mismatch (expected {}, got {})",
                        expected_model.name, ef.name, e_uniq, a_uniq
                    ));
                }

                let e_null = ef.has(crate::types::Intent::Null);
                let a_null = af.has(crate::types::Intent::Null);
                if e_null != a_null {
                    return Err(format!(
                        "field `{}.{}` nullability mismatch (expected {}, got {})",
                        expected_model.name, ef.name, e_null, a_null
                    ));
                }
            }
        }
        Ok(())
    }

    /// Like [`Ast::schema_matches`] but additionally compares column type
    /// keywords, so a silent type change (e.g. `int` → `bigint`) surfaces as an
    /// error instead of being masked by the structural-only comparison.
    pub fn schema_matches_strict(&self, actual: &Ast) -> Result<(), String> {
        let actual_models: std::collections::HashMap<String, &Model> =
            actual.models().map(|(_, m)| (m.name.clone(), m)).collect();

        for (_, expected_model) in self.models() {
            let actual_model = actual_models.get(&expected_model.name).ok_or_else(|| {
                format!("missing model `{}` in live database", expected_model.name)
            })?;

            let actual_fields: std::collections::HashMap<String, &Field> = actual_model
                .fields()
                .map(|(_, f)| (f.name.clone(), f))
                .collect();

            for (_, ef) in expected_model.fields() {
                let af = actual_fields.get(&ef.name).ok_or_else(|| {
                    format!(
                        "missing field `{}.{}` in live database",
                        expected_model.name, ef.name
                    )
                })?;

                let e_pk = ef.has(crate::types::Intent::Pk);
                let a_pk = af.has(crate::types::Intent::Pk);
                if e_pk != a_pk {
                    return Err(format!(
                        "field `{}.{}` primary-key mismatch (expected {}, got {})",
                        expected_model.name, ef.name, e_pk, a_pk
                    ));
                }

                let e_uniq = ef.has(crate::types::Intent::Uniq);
                let a_uniq = af.has(crate::types::Intent::Uniq);
                if e_uniq != a_uniq {
                    return Err(format!(
                        "field `{}.{}` unique mismatch (expected {}, got {})",
                        expected_model.name, ef.name, e_uniq, a_uniq
                    ));
                }

                let e_null = ef.has(crate::types::Intent::Null);
                let a_null = af.has(crate::types::Intent::Null);
                if e_null != a_null {
                    return Err(format!(
                        "field `{}.{}` nullability mismatch (expected {}, got {})",
                        expected_model.name, ef.name, e_null, a_null
                    ));
                }

                let e_ty = ef.type_keyword();
                let a_ty = af.type_keyword();
                if e_ty != a_ty {
                    return Err(format!(
                        "field `{}.{}` type mismatch (expected `{}`, got `{}`)",
                        expected_model.name, ef.name, e_ty, a_ty
                    ));
                }
            }
        }
        Ok(())
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
    /// Composite primary key declared with a model-level `-pk a,b` line.
    /// When non-empty, these field names form the (composite) primary key and
    /// individual fields must NOT carry `-pk`. When empty, the primary key is
    /// taken from fields carrying the `-pk` intent (legacy single-column form).
    pub primary_key: Vec<String>,
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
    /// Index method/type for engines that support it (`gin`, `gist`, `btree`,
    /// `hash`, ...). Emitted as Postgres `USING <type>` / SeaQuery
    /// `IndexType`.
    pub index_type: Option<String>,
    /// MySQL prefix length for the (first) indexed column, e.g. `-prefix 10`.
    pub prefix_length: Option<u16>,
    /// Partial-index predicate (`-where "deleted_at IS NULL"`). Emitted as a
    /// `WHERE` clause on the index.
    pub where_clause: Option<String>,
    /// Column sort order for the (first) indexed column: `asc` or `desc`.
    pub order: Option<String>,
    /// `NULLS FIRST` / `NULLS LAST` placement (Postgres).
    pub nulls: Option<String>,
}

/// A read-model / projection declared with a top-level `view` block.
///
/// ```text
/// view UserSummary (id, email, post_count) -materialized =
///   SELECT u.id, u.email, COUNT(p.id)
///   FROM users u LEFT JOIN posts p ON p.author_id = u.id
///   GROUP BY u.id, u.email
/// ```
///
/// A view carries an explicit column projection (`fields`) — used for
/// typed read-models in codegen — plus the raw query body (`query`) that
/// becomes the `AS` clause of a SQL `CREATE VIEW`. `-materialized` marks a
/// `CREATE MATERIALIZED VIEW` (Postgres; other dialects downgrade to a plain
/// view or fail at apply time). Views are stored as a *separate* top-level
/// collection from `Model`s so no model/field/lockfile literal breaks and the
/// two namespaces never collide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub name: String,
    /// Declared output columns (name + type). May be empty when the schema
    /// relies on the query's own projection; codegen still emits a typed row
    /// when present.
    pub fields: Vec<ViewField>,
    /// The SELECT body (everything after `=`), emitted verbatim into the view.
    pub query: String,
    /// Whether this is a `CREATE MATERIALIZED VIEW` (Postgres) vs plain.
    pub materialized: bool,
    pub line: usize,
}

/// A single column of a [`View`] projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewField {
    pub name: String,
    /// Scalar type keyword (`uuid`, `string`, `int`, ...) or a model reference
    /// `Model.field`. Same vocabulary as a model field's `ty`.
    pub ty: String,
}

/// Lockfile projection of a [`View`] (deterministic, serializable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockView {
    pub name: String,
    pub fields: Vec<(String, String)>,
    pub query: String,
    pub materialized: bool,
}

impl Ast {
    /// Project the views into their deterministic lockfile form (fields sorted
    /// by name, views sorted by name) for diffing / migration planning.
    pub fn to_lock_views(&self) -> Vec<LockView> {
        let mut views: Vec<LockView> = self
            .views
            .iter()
            .map(|v| {
                let mut fields: Vec<(String, String)> = v
                    .fields
                    .iter()
                    .map(|f| (f.name.clone(), f.ty.clone()))
                    .collect();
                fields.sort();
                LockView {
                    name: v.name.clone(),
                    fields,
                    query: v.query.clone(),
                    materialized: v.materialized,
                }
            })
            .collect();
        views.sort_by(|a, b| a.name.cmp(&b.name));
        views
    }
}

/// Lockfile projection of a model-level index (see [`ModelIndex`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockIndex {
    pub name: String,
    pub fields: Vec<String>,
    pub unique: bool,
    pub index_type: Option<String>,
    pub prefix_length: Option<u16>,
    pub where_clause: Option<String>,
    pub order: Option<String>,
    pub nulls: Option<String>,
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

    /// Resolve the primary-key column names for this model.
    ///
    /// A model-level `-pk a,b` declaration takes precedence (composite keys).
    /// Otherwise the key is the set of fields carrying the `-pk` intent
    /// (the legacy single-column form). Always returns at least one column
    /// for a model that passed validation.
    pub fn pk_columns(&self) -> Vec<String> {
        if !self.primary_key.is_empty() {
            return self.primary_key.clone();
        }
        self.fields
            .iter()
            .filter(|(_, f)| f.has(crate::types::Intent::Pk))
            .map(|(_, f)| f.name.clone())
            .collect()
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
    /// Name of the custom type (`type X = ...`) this field was declared with,
    /// if any. Used by renderers that emit newtype wrappers.
    pub custom_type: Option<String>,
    /// Referential action on deletion of the referenced row (`-ondelete`).
    /// `None` means "unspecified" and the SQL layer falls back to `Cascade`
    /// (preserving historic behaviour).
    pub on_delete: Option<FkAction>,
    /// Referential action on update of the referenced key (`-onupdate`).
    pub on_update: Option<FkAction>,
    /// Numeric precision (total digits), set by `-precision N` on `numeric`.
    pub numeric_precision: Option<u16>,
    /// Numeric scale (fractional digits), set by `-scale N` on `numeric`.
    pub numeric_scale: Option<u16>,
    /// Explicit join model for a `-through` relation (`posts -relation Post
    /// -through PostAuthor`). When set, the auto-generated `<Source>_<Target>`
    /// junction is suppressed and the named model is used as the join table.
    pub through_model: Option<String>,
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
    /// Primary-key column names. For a model-level `-pk a,b` this holds all
    /// declared columns (composite). For legacy single-column PKs it holds the
    /// one field carrying `-pk`. Always non-empty for a valid model.
    pub primary_key: Vec<String>,
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
    /// Explicit join model for a `-through` relation; `None` when absent.
    pub through_model: Option<String>,
    /// Referential action on deletion of the referenced row (`-ondelete`);
    /// `None` when unspecified.
    pub on_delete: Option<String>,
    /// Referential action on update of the referenced key (`-onupdate`);
    /// `None` when unspecified.
    pub on_update: Option<String>,
    /// Numeric precision (total digits) for `numeric(p,s)` columns.
    pub numeric_precision: Option<u16>,
    /// Numeric scale (fractional digits) for `numeric(p,s)` columns.
    pub numeric_scale: Option<u16>,
}

impl LockModel {
    pub fn field_by_name(&self, name: &str) -> Option<&LockField> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// Primary-key column names (composite when `primary_key` has >1 entry).
    /// Falls back to fields carrying the per-field `-pk` intent for the legacy
    /// single-column-key form.
    pub fn pk_columns(&self) -> Vec<String> {
        if !self.primary_key.is_empty() {
            return self.primary_key.clone();
        }
        self.fields
            .iter()
            .filter(|f| f.intents.iter().any(|i| i == "-pk"))
            .map(|f| f.name.clone())
            .collect()
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
                            on_delete: f.on_delete.map(|a| a.as_keyword().to_string()),
                            on_update: f.on_update.map(|a| a.as_keyword().to_string()),
                            numeric_precision: f.numeric_precision,
                            numeric_scale: f.numeric_scale,
                            through_model: f.through_model.clone(),
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
                        index_type: i.index_type.clone(),
                        prefix_length: i.prefix_length,
                        where_clause: i.where_clause.clone(),
                        order: i.order.clone(),
                        nulls: i.nulls.clone(),
                    })
                    .collect();
                indexes.sort_by(|a, b| a.name.cmp(&b.name));
                LockModel {
                    name: m.name.clone(),
                    fields,
                    indexes,
                    primary_key: m.pk_columns(),
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
                // An explicit `-through <Join>` join model means the user owns
                // the junction table; suppress the auto-generated
                // `<Source>_<Target>` and just drop the virtual accessor field.
                if m.fields[i].through_model.is_none() {
                    junctions.push((m.name.clone(), target));
                }
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
                on_delete: None,
                on_update: None,
                numeric_precision: None,
                numeric_scale: None,
                through_model: None,
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
                on_delete: None,
                on_update: None,
                numeric_precision: None,
                numeric_scale: None,
                through_model: None,
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
                on_delete: None,
                on_update: None,
                numeric_precision: None,
                numeric_scale: None,
                through_model: None,
            },
        ];
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        let indexes = vec![LockIndex {
            name: format!("uniq_{source_l}_id_{target_l}_id"),
            fields: vec![format!("{source_l}_id"), format!("{target_l}_id")],
            unique: true,
            index_type: None,
            prefix_length: None,
            where_clause: None,
            order: None,
            nulls: None,
        }];
        models.push(LockModel {
            name: jname,
            fields,
            indexes,
            primary_key: vec![format!("{source_l}_id"), format!("{target_l}_id")],
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
        let idx = ast.add_model(user);
        assert_eq!(ast.model_by_name("User"), Some(idx));
        let u = &ast.models[idx];
        assert!(u.field_by_name("id").is_some());
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
            primary_key: vec![],
        };
        a.add_field(Field {
            custom_type: None,
            name: "name".into(),
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
            line: 1,
        });
        let mut b = Model {
            name: "Apple".into(),
            fields: Arena::new(),
            field_index: vec![],
            line: 1,
            indexes: vec![],
            primary_key: vec![],
        };
        b.add_field(Field {
            custom_type: None,
            name: "id".into(),
            ty: FieldType::Scalar(ScalarType::Int),
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
        b.add_field(Field {
            custom_type: None,
            name: "z".into(),
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
            line: 2,
        });
        ast.add_model(a);
        ast.add_model(b);
        let lock = ast.to_lock();
        assert_eq!(lock[0].name, "Apple");
        assert_eq!(lock[1].name, "Zebra");
        assert_eq!(lock[0].fields[0].name, "id"); // sorted: id before z
    }

    /// Build a model with a single pk `id` plus the given extra fields.
    fn model_with(name: &str, fields: Vec<(&str, ScalarType, Vec<Intent>)>) -> Model {
        let mut m = Model {
            name: name.into(),
            fields: Arena::new(),
            field_index: vec![],
            line: 1,
            indexes: vec![],
            primary_key: vec![],
        };
        m.add_field(Field {
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
        let mut line = 2;
        for (fname, ty, intents) in fields {
            m.add_field(Field {
                custom_type: None,
                name: fname.into(),
                ty: FieldType::Scalar(ty),
                intents,
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
                line,
            });
            line += 1;
        }
        m
    }

    #[test]
    fn schema_matches_ignores_type_strings() {
        // `expected` uses `int`, `actual` uses `bigint` — types differ but
        // names / pk / uniq / nullability agree, so they "match".
        let mut expected = Ast::new();
        expected.add_model(model_with(
            "User",
            vec![
                ("age", ScalarType::Int, vec![]),
                ("email", ScalarType::String, vec![Intent::Uniq]),
            ],
        ));

        let mut actual = Ast::new();
        actual.add_model(model_with(
            "User",
            vec![
                ("age", ScalarType::BigInt, vec![]),
                ("email", ScalarType::String, vec![Intent::Uniq]),
            ],
        ));
        assert!(expected.schema_matches(&actual).is_ok());
    }

    #[test]
    fn schema_matches_detects_nullability_mismatch() {
        let mut expected = Ast::new();
        expected.add_model(model_with(
            "User",
            vec![("age", ScalarType::Int, vec![Intent::Null])],
        ));
        let mut actual = Ast::new();
        // actual has age as NOT null -> mismatch
        actual.add_model(model_with("User", vec![("age", ScalarType::Int, vec![])]));
        assert!(expected.schema_matches(&actual).is_err());
    }

    #[test]
    fn schema_matches_detects_missing_field() {
        let mut expected = Ast::new();
        expected.add_model(model_with(
            "User",
            vec![("email", ScalarType::String, vec![])],
        ));
        let mut actual = Ast::new();
        // actual User has only id, missing `email`
        actual.add_model(model_with("User", vec![]));
        assert!(expected.schema_matches(&actual).is_err());
    }

    #[test]
    fn schema_matches_strict_detects_type_drift() {
        // `expected` uses `int`, `actual` uses `bigint`. The structural check
        // ignores types, but strict mode must surface the drift as an error.
        let mut expected = Ast::new();
        expected.add_model(model_with(
            "User",
            vec![
                ("age", ScalarType::Int, vec![]),
                ("email", ScalarType::String, vec![Intent::Uniq]),
            ],
        ));
        let mut actual = Ast::new();
        actual.add_model(model_with(
            "User",
            vec![
                ("age", ScalarType::BigInt, vec![]),
                ("email", ScalarType::String, vec![Intent::Uniq]),
            ],
        ));
        assert!(expected.schema_matches(&actual).is_ok());
        assert!(expected.schema_matches_strict(&actual).is_err());
    }
}
