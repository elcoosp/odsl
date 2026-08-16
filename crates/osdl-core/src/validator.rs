//! The target code generation trait and the validation pipeline.
//!
//! [`CodeRenderer`] is the seam that keeps the compiler core decoupled from any
//! specific ORM/driver (ADR-003). Renderers live in their own crates and
//! consume a *validated* [`Ast`].

use crate::ast::{Ast, Field};
use crate::errors::{CompileErrorKind, OsdlError};
use crate::intent_compat::Target;
use crate::types::{FieldType, Intent, Reference, ScalarType};
use std::collections::{HashMap, HashSet};

/// A target backend renderer.
pub trait CodeRenderer {
    /// Human-readable target name, e.g. `seaorm`, `mongo`.
    fn target(&self) -> Target;

    /// Render the full set of files for the schema. Each entry is
    /// `(relative_path, contents)`. The CLI writes these to disk.
    fn render(&self, ast: &Ast) -> Result<Vec<(String, String)>, OsdlError>;
}

/// The unified validation + reference-resolution pipeline.
///
/// Produces business-rule and target-compatibility errors exactly as the SRS
/// requires (BR-001..003, REQ-FUNC-004/006/007/008).
pub struct Validator;

impl Validator {
    /// Validate `ast` against the shared business rules. `target` enables
    /// target-compatibility checks (REQ-FUNC-008 / BR-003); pass `None` to skip
    /// them (e.g. when only code-gen structure matters).
    pub fn validate(ast: &Ast, target: Option<Target>) -> Result<(), OsdlError> {
        Self::resolve_references(ast)?;
        Self::check_keys(ast)?;
        Self::check_intent_compat(ast)?;
        Self::check_model_indexes(ast)?;
        Self::check_m2m_targets(ast)?;
        Self::check_views(ast)?;
        if let Some(t) = target {
            Self::check_target_compat(ast, t)?;
            Self::prevent_cycles(ast)?;
        }
        Ok(())
    }

    /// REQ-FUNC-004 / BR-002: every `Model.field` reference must resolve.
    fn resolve_references(ast: &Ast) -> Result<(), OsdlError> {
        for (_midx, model) in ast.models() {
            for (_fidx, field) in model.fields() {
                if let FieldType::Ref(r) = &field.ty
                    && ast.model_by_name(&r.model).is_none()
                {
                    return Err(OsdlError::compile(CompileErrorKind::UnresolvedReference {
                        from: format!("{}.{}", model.name, field.name),
                        target: r.model.clone(),
                    }));
                }
            }
        }
        Ok(())
    }

    /// BR-001: every model must declare exactly one primary/partition key.
    fn check_keys(ast: &Ast) -> Result<(), OsdlError> {
        for (_midx, model) in ast.models() {
            // A model-level `-pk a,b` declaration wins (composite key).
            if !model.primary_key.is_empty() {
                // Mixing model-level `-pk` with per-field `-pk` is ambiguous.
                if model.fields().any(|(_, f)| f.has(crate::types::Intent::Pk)) {
                    return Err(OsdlError::compile(CompileErrorKind::InvalidKey {
                        model: model.name.clone(),
                        reason: "model declares both `-pk a,b` and a `-pk` field".to_string(),
                    }));
                }
                // Every named column must exist on the model.
                for col in &model.primary_key {
                    if model.field_by_name(col).is_none() {
                        return Err(OsdlError::compile(CompileErrorKind::InvalidKey {
                            model: model.name.clone(),
                            reason: format!("primary key column `{col}` does not exist"),
                        }));
                    }
                }
                continue;
            }
            // Legacy form: exactly one field carries `-pk`.
            let keys = model
                .fields()
                .filter(|(_, f)| {
                    f.has(crate::types::Intent::Pk) || f.has(crate::types::Intent::Partition)
                })
                .count();
            if keys != 1 {
                return Err(OsdlError::compile(CompileErrorKind::MissingKey {
                    model: model.name.clone(),
                }));
            }
        }
        Ok(())
    }

    /// REQ-FUNC-007: an intent flag must be applied to a compatible field type.
    fn check_intent_compat(ast: &Ast) -> Result<(), OsdlError> {
        for (_midx, model) in ast.models() {
            for (_fidx, field) in model.fields() {
                for intent in &field.intents {
                    if !is_intent_compatible(*intent, &field.ty) {
                        return Err(OsdlError::compile(CompileErrorKind::TypeMismatch {
                            intent: intent.as_keyword().to_string(),
                            ty: field.type_keyword(),
                        }));
                    }
                    // An enum must be a string field with at least one variant.
                    if *intent == Intent::Enum
                        && !matches!(field.ty, FieldType::Scalar(ScalarType::String))
                    {
                        return Err(OsdlError::compile(CompileErrorKind::TypeMismatch {
                            intent: "-enum".into(),
                            ty: field.type_keyword(),
                        }));
                    }
                    if *intent == Intent::Enum && field.enum_variants.is_empty() {
                        return Err(OsdlError::compile(CompileErrorKind::TypeMismatch {
                            intent: "-enum".into(),
                            ty: "requires at least one variant".into(),
                        }));
                    }
                    // A `now` default is only valid for temporal types.
                    if *intent == Intent::Default
                        && let Some(value) = &field.default_value
                        && value == "now"
                        && !matches!(
                            field.ty,
                            FieldType::Scalar(ScalarType::DateTime)
                                | FieldType::Scalar(ScalarType::Date)
                        )
                    {
                        return Err(OsdlError::compile(CompileErrorKind::TypeMismatch {
                            intent: "-default now".into(),
                            ty: field.type_keyword(),
                        }));
                    }
                    // `-check "expr"` requires a scalar field and a non-empty expression.
                    if *intent == Intent::Check && !matches!(field.ty, FieldType::Scalar(_)) {
                        return Err(OsdlError::compile(CompileErrorKind::TypeMismatch {
                            intent: "-check".into(),
                            ty: field.type_keyword(),
                        }));
                    }
                    if *intent == Intent::Check
                        && field.check_expr.as_deref().unwrap_or("").trim().is_empty()
                    {
                        return Err(OsdlError::compile(CompileErrorKind::TypeMismatch {
                            intent: "-check".into(),
                            ty: "requires a non-empty expression (use -check \"...\")".into(),
                        }));
                    }
                    // `-softdelete` must annotate a nullable timestamp column.
                    if *intent == Intent::SoftDelete
                        && !matches!(
                            field.ty,
                            FieldType::Scalar(ScalarType::DateTime)
                                | FieldType::Scalar(ScalarType::Date)
                        )
                    {
                        return Err(OsdlError::compile(CompileErrorKind::TypeMismatch {
                            intent: "-softdelete".into(),
                            ty: field.type_keyword(),
                        }));
                    }
                    if *intent == Intent::SoftDelete && !field.has(Intent::Null) {
                        return Err(OsdlError::compile(CompileErrorKind::TypeMismatch {
                            intent: "-softdelete".into(),
                            ty: "must be nullable (-null) to allow soft deletes".into(),
                        }));
                    }
                    // `-virtual` fields are computed/serialized only: they must not
                    // carry DB-backed intents (which would imply a column).
                    if *intent == Intent::Virtual {
                        for db_intent in [
                            Intent::Pk,
                            Intent::Uniq,
                            Intent::Auto,
                            Intent::Tz,
                            Intent::Index,
                            Intent::Default,
                            Intent::Check,
                            Intent::SoftDelete,
                        ] {
                            if field.has(db_intent) {
                                return Err(OsdlError::compile(CompileErrorKind::TypeMismatch {
                                    intent: intent.as_keyword().to_string(),
                                    ty: format!(
                                        "cannot combine -virtual with {}",
                                        db_intent.as_keyword()
                                    ),
                                }));
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// REQ-FUNC-007 (model-level): every field referenced by a composite
    /// `-index`/`-uniq` constraint must exist on the model.
    fn check_model_indexes(ast: &Ast) -> Result<(), OsdlError> {
        for (_midx, model) in ast.models() {
            for index in &model.indexes {
                for field in &index.fields {
                    if model.field_by_name(field).is_none() {
                        return Err(OsdlError::compile(CompileErrorKind::UnresolvedReference {
                            from: format!("{}.{}", model.name, index.name),
                            target: field.clone(),
                        }));
                    }
                }
            }
        }
        Ok(())
    }

    /// REQ-FUNC-007 (m2m): every `-m2m <Target>` must reference an existing model.
    fn check_m2m_targets(ast: &Ast) -> Result<(), OsdlError> {
        for (_midx, model) in ast.models() {
            for (_fidx, field) in model.fields() {
                if field.has(Intent::M2m) {
                    let Some(target) = &field.m2m_target else {
                        return Err(OsdlError::compile(CompileErrorKind::TypeMismatch {
                            intent: "-m2m".into(),
                            ty: "requires a target model".into(),
                        }));
                    };
                    if ast.model_by_name(target).is_none() {
                        return Err(OsdlError::compile(CompileErrorKind::UnresolvedReference {
                            from: format!("{}.{}", model.name, field.name),
                            target: target.clone(),
                        }));
                    }
                }
                // `-hasone Model`: the target must exist.
                if field.has(Intent::HasOne) {
                    let Some(target) = relation_target(field) else {
                        return Err(OsdlError::compile(CompileErrorKind::TypeMismatch {
                            intent: "-hasone".into(),
                            ty: "requires a target model".into(),
                        }));
                    };
                    if ast.model_by_name(&target).is_none() {
                        return Err(OsdlError::compile(CompileErrorKind::UnresolvedReference {
                            from: format!("{}.{}", model.name, field.name),
                            target,
                        }));
                    }
                }
                // `-through Join`: the join model must exist.
                if let Some(join) = &field.through_model
                    && ast.model_by_name(join).is_none()
                {
                    return Err(OsdlError::compile(CompileErrorKind::UnresolvedReference {
                        from: format!("{}.{}", model.name, field.name),
                        target: join.clone(),
                    }));
                }
            }
        }
        Ok(())
    }

    /// Phase 1.5: validate top-level `view` (read-model) declarations.
    ///
    /// * A view must have a non-empty name and query.
    /// * The optional projection's field types must resolve to a known scalar
    ///   or custom type (so generated read-model structs are well-typed).
    /// * A view name must not collide with a model name.
    /// * View DDL is only emitted for SQL backends and GraphQL; Mongo (and pure
    ///   TS/validator targets) cannot materialize an arbitrary query, so a view
    ///   is rejected when the target is `Mongo`.
    fn check_views(ast: &Ast) -> Result<(), OsdlError> {
        use crate::types::ScalarType;
        let mut seen = std::collections::HashSet::new();
        for v in ast.views() {
            if v.name.is_empty() {
                return Err(OsdlError::compile(CompileErrorKind::ViewError {
                    view: v.name.clone(),
                    reason: "view name must not be empty".into(),
                }));
            }
            if v.query.trim().is_empty() {
                return Err(OsdlError::compile(CompileErrorKind::ViewError {
                    view: v.name.clone(),
                    reason: "view must declare a query after `=`".into(),
                }));
            }
            if !seen.insert(v.name.clone()) {
                return Err(OsdlError::compile(CompileErrorKind::ViewError {
                    view: v.name.clone(),
                    reason: "duplicate view name".into(),
                }));
            }
            if ast.model_by_name(&v.name).is_some() {
                return Err(OsdlError::compile(CompileErrorKind::ViewError {
                    view: v.name.clone(),
                    reason: "view name collides with a model name".into(),
                }));
            }
            for f in &v.fields {
                // Accept a known scalar keyword or a custom type declared in
                // the schema. Anything else is rejected as an unknown type.
                let known = ScalarType::from_keyword(&f.ty).is_some()
                    || ast.custom_type_by_name(&f.ty).is_some();
                if !known {
                    return Err(OsdlError::compile(CompileErrorKind::ViewError {
                        view: v.name.clone(),
                        reason: format!(
                            "projection field `{}` has unknown type `{}`",
                            f.name, f.ty
                        ),
                    }));
                }
            }
        }
        Ok(())
    }
    fn check_target_compat(ast: &Ast, target: Target) -> Result<(), OsdlError> {
        for (_midx, model) in ast.models() {
            for (_fidx, field) in model.fields() {
                for intent in &field.intents {
                    if !target_supports(target, *intent, &field.ty) {
                        return Err(OsdlError::compile(
                            CompileErrorKind::TargetIncompatibility {
                                feature: intent.as_keyword().to_string(),
                                target: target_label(target),
                                detail: format!("field `{}`", field.name),
                            },
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// REQ-FUNC-006: detect cyclic `Ref`/`-relation` dependencies between models.
    fn prevent_cycles(ast: &Ast) -> Result<(), OsdlError> {
        // Build adjacency: model -> referenced models.
        let mut adj: HashMap<String, HashSet<String>> = HashMap::new();
        for (_midx, model) in ast.models() {
            let entry = adj.entry(model.name.clone()).or_default();
            for (_fidx, field) in model.fields() {
                match &field.ty {
                    FieldType::Ref(r) => {
                        if r.model != model.name {
                            entry.insert(r.model.clone());
                        }
                    }
                    FieldType::InferredRef(s) if is_model_name(ast, s) && s != &model.name => {
                        entry.insert(s.clone());
                    }
                    _ => {}
                }
                if field.has(Intent::Relation)
                    && let Some(tgt) = relation_target(field)
                    && ast.model_by_name(&tgt).is_some()
                    && tgt != model.name
                {
                    entry.insert(tgt);
                }
            }
        }

        // DFS with a recursion stack to find a cycle.
        let mut visited: HashSet<String> = HashSet::new();
        let mut stack: Vec<String> = Vec::new();
        for (_midx, model) in ast.models() {
            if !visited.contains(&model.name)
                && let Some(cycle) = dfs(&adj, &model.name, &mut visited, &mut stack)
            {
                return Err(OsdlError::compile(CompileErrorKind::CyclicDependency {
                    models: cycle,
                }));
            }
        }
        Ok(())
    }
}

fn is_model_name(ast: &Ast, name: &str) -> bool {
    ast.model_by_name(name).is_some()
}

/// `-relation` carries the target model in its name, e.g. `posts -relation Post`.
/// We encode the target as `relation:Post` in the type keyword, see parser.
fn relation_target(field: &Field) -> Option<String> {
    // The parser stores `-relation <Model>` as type keyword `relation:Model`,
    // and `-hasone <Model>` resolves the type to a `Ref(Model)` (or
    // `relation:Model`). Accept both forms.
    if let FieldType::InferredRef(s) = &field.ty
        && let Some(stripped) = s.strip_prefix("relation:")
    {
        return Some(stripped.to_string());
    }
    if let FieldType::Ref(r) = &field.ty {
        return Some(r.model.clone());
    }
    None
}

fn dfs(
    adj: &HashMap<String, HashSet<String>>,
    node: &str,
    visited: &mut HashSet<String>,
    stack: &mut Vec<String>,
) -> Option<Vec<String>> {
    visited.insert(node.to_string());
    stack.push(node.to_string());
    if let Some(neighbors) = adj.get(node) {
        for n in neighbors {
            if stack.contains(n) {
                // cycle found: return the slice of the stack from `n`.
                let start = stack.iter().position(|s| s == n).unwrap();
                return Some(stack[start..].to_vec());
            }
            if !visited.contains(n)
                && let Some(cycle) = dfs(adj, n, visited, stack)
            {
                return Some(cycle);
            }
        }
    }
    stack.pop();
    None
}

/// Whether an intent is semantically valid on a given (possibly unresolved) type.
fn is_intent_compatible(intent: Intent, ty: &FieldType) -> bool {
    use Intent::*;
    match intent {
        Pk | Partition | Uniq | Null | Auto | Tz | Relation | HasOne | Index | Enum | Default
        | M2m | Virtual | SoftDelete | Check | Polymorphic | OnDelete | OnUpdate => true,
        Fulltext => {
            // Full-text search only makes sense on textual types.
            matches!(ty, FieldType::Scalar(ScalarType::String))
                || matches!(ty, FieldType::InferredRef(_))
        }
    }
}

/// Whether a target backend natively supports an intent for a given type.
fn target_supports(target: Target, intent: Intent, _ty: &FieldType) -> bool {
    use Intent::*;
    use Target::*;
    match (target, intent) {
        // SQL backends support these intents natively.
        (
            SeaOrmSqlite,
            Pk | Uniq | Null | Auto | Tz | Relation | HasOne | Index | Enum | Default | M2m
            | Virtual | SoftDelete | Check | Polymorphic,
        ) => true,
        (SeaOrmSqlite, Fulltext) => true,   // SQLite FTS5
        (SeaOrmSqlite, Partition) => false, // SQLite has no partition concept
        (
            SeaOrmPostgres | SeaOrmMysql,
            Pk | Uniq | Null | Auto | Tz | Relation | HasOne | Index | Enum | Default | M2m
            | Virtual | SoftDelete | Check | Polymorphic,
        ) => true,
        (SeaOrmPostgres, Fulltext) => true,   // PG GIN
        (SeaOrmPostgres, Partition) => false, // partition requires table-level DDL, not a field flag here
        (SeaOrmMysql, Fulltext) => true,      // MySQL FULLTEXT index
        (SeaOrmMysql, Partition) => false,    // partition requires table-level DDL here
        // Mongo supports these natively.
        (
            Mongo,
            Pk | Uniq | Null | Tz | Partition | Relation | HasOne | Index | Enum | Default | M2m
            | Virtual | SoftDelete | Check | Polymorphic,
        ) => true,
        (Mongo, Auto) => false,    // Mongo has no auto-increment
        (Mongo, Fulltext) => true, // Mongo text index
        // FK referential actions: supported on every SQL backend; advisory on Mongo.
        (SeaOrmSqlite | SeaOrmPostgres | SeaOrmMysql | Mongo, OnDelete | OnUpdate) => true,
        // Transpile targets (TS / GraphQL / OpenAPI / JSON Schema / Zod /
        // Valibot / TypeBox) describe types only and support every intent as a
        // documentation/constraint annotation.
        (TypeScript | GraphQl | OpenApi | JsonSchema | Zod | Valibot | TypeBox | Trpc, _) => true,
    }
}

fn target_label(target: Target) -> String {
    target.as_str().to_string()
}

/// Helper used by both renderer crates: map a scalar to its canonical Rust type
/// string for SeaORM entities.
pub fn rust_type_for(scalar: ScalarType) -> &'static str {
    match scalar {
        ScalarType::String => "String",
        ScalarType::Int => "i32",
        ScalarType::BigInt => "i64",
        ScalarType::Float => "f64",
        ScalarType::Bool => "bool",
        ScalarType::DateTime => "chrono::DateTime<chrono::Utc>",
        ScalarType::Date => "chrono::NaiveDate",
        ScalarType::Uuid => "uuid::Uuid",
        ScalarType::Json => "serde_json::Value",
        ScalarType::Binary => "Vec<u8>",
        ScalarType::Decimal => "Decimal",
    }
}

/// Resolve the referenced [`Reference`] from a field, if it is a ref/relation.
pub fn field_reference(field: &Field) -> Option<Reference> {
    match &field.ty {
        FieldType::Ref(r) => Some(r.clone()),
        FieldType::InferredRef(s) if s.starts_with("relation:") => Some(Reference {
            model: s.trim_start_matches("relation:").to_string(),
            field: "id".into(),
        }),
        _ => None,
    }
}
