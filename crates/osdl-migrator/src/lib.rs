//! OSDL migration engine.
//!
//! The migrator compares two schema snapshots — a *current* [`Lockfile`] (what
//! is deployed, from `osdl.lock`) and a *target* [`Ast`] (the new `.osdl` after
//! validation) — and produces a deterministic, ordered list of
//! [`MigrationOp`]s. Each op carries enough metadata for a backend adapter
//! (SeaORM `sea-orm-migration`, MongoDB `$jsonschema` validators) to apply it.
//!
//! Determinism: ops are emitted in a stable order (drops last, creates first)
//! so the same schema delta always yields the same migration plan.

#![allow(clippy::result_large_err)]

use osdl_core::ast::{Ast, LockField, LockModel};
use osdl_core::errors::OsdlError;
use osdl_core::lockfile::Lockfile;
use osdl_core::Target;
use std::collections::BTreeMap;

/// A single, backend-agnostic schema change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationOp {
    /// Create a model/collection that did not exist before.
    CreateModel { model: String },
    /// Drop a model/collection that no longer exists.
    DropModel { model: String },
    /// Add a column/field to an existing model.
    AddField {
        model: String,
        field: String,
        ty: String,
        nullable: bool,
        uniq: bool,
    },
    /// Drop a column/field from a model.
    DropField { model: String, field: String },
    /// Alter a column/field (type or constraint change).
    AlterField {
        model: String,
        field: String,
        new_ty: String,
        nullable: bool,
        uniq: bool,
    },
}

/// The ordered plan describing how to move `from` to `to`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MigrationPlan {
    pub ops: Vec<MigrationOp>,
}

impl MigrationPlan {
    /// Build a plan by diffing two lockfiles.
    pub fn diff(from: &Lockfile, to: &Lockfile) -> Self {
        let mut ops = Vec::new();

        let from_models: BTreeMap<&str, &LockModel> =
            from.models.iter().map(|m| (m.name.as_str(), m)).collect();
        let to_models: BTreeMap<&str, &LockModel> =
            to.models.iter().map(|m| (m.name.as_str(), m)).collect();

        // Created models.
        for name in to_models.keys() {
            if !from_models.contains_key(name) {
                ops.push(MigrationOp::CreateModel {
                    model: (*name).to_string(),
                });
            }
        }

        // Dropped models (emit last so dependents are handled first).
        // Collected separately and appended after field ops.
        let mut drops = Vec::new();

        // Compare shared models.
        for (name, tm) in &to_models {
            if let Some(fm) = from_models.get(name) {
                diff_fields(fm, tm, &mut ops);
            }
        }

        for name in from_models.keys() {
            if !to_models.contains_key(name) {
                drops.push(MigrationOp::DropModel {
                    model: (*name).to_string(),
                });
            }
        }
        ops.extend(drops);

        Self { ops }
    }

    /// Render a stable, human-readable description of the plan.
    pub fn describe(&self) -> Vec<String> {
        self.ops
            .iter()
            .map(|op| match op {
                MigrationOp::CreateModel { model } => format!("create model {model}"),
                MigrationOp::DropModel { model } => format!("drop model {model}"),
                MigrationOp::AddField { model, field, .. } => {
                    format!("add field {model}.{field}")
                }
                MigrationOp::DropField { model, field } => {
                    format!("drop field {model}.{field}")
                }
                MigrationOp::AlterField { model, field, .. } => {
                    format!("alter field {model}.{field}")
                }
            })
            .collect()
    }

    /// Ops that can destroy data: dropping models/fields and altering a
    /// column's type or constraints. AddField is non-destructive.
    pub fn destructive_ops(&self) -> Vec<&MigrationOp> {
        self.ops
            .iter()
            .filter(|op| match op {
                MigrationOp::DropModel { .. }
                | MigrationOp::DropField { .. }
                | MigrationOp::AlterField { .. } => true,
                MigrationOp::CreateModel { .. } | MigrationOp::AddField { .. } => false,
            })
            .collect()
    }

    /// Whether the plan contains any data-destroying operation.
    pub fn is_destructive(&self) -> bool {
        !self.destructive_ops().is_empty()
    }

    /// Zero-downtime / safety advisories for the plan on a given backend target.
    ///
    /// These are *warnings*, not errors: the migration is still valid to apply,
    /// but the listed ops may lock tables or risk data depending on the engine.
    /// Returns a list of `(operation-index, advisory-message)` pairs.
    pub fn advisories(&self, target: Target) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        for (i, op) in self.ops.iter().enumerate() {
            match op {
                MigrationOp::AddField {
                    model,
                    field,
                    nullable,
                    ..
                } => {
                    // Postgres/MySQL: adding a non-nullable column without a
                    // default rewrites the whole table (long lock on big tables).
                    if !*nullable
                        && matches!(
                            target,
                            Target::SeaOrmPostgres | Target::SeaOrmMysql
                        )
                    {
                        out.push((
                            i,
                            format!(
                                "adding non-nullable column {model}.{field} on {} without a \
                                 default will rewrite the entire table (table lock); add a \
                                 -default or make it -null to deploy online",
                                target_dialect_name(target)
                            ),
                        ));
                    }
                }
                MigrationOp::AlterField { model, field, .. } => {
                    // Changing type or constraints requires a table rewrite on
                    // every engine; generally unsafe to do online.
                    out.push((
                        i,
                        format!(
                            "altering {model}.{field} rewrites the column and may lock the \
                             table; prefer expand-and-contract (add new column, backfill, drop \
                             old) for zero-downtime"
                        ),
                    ));
                }
                MigrationOp::DropModel { model } => {
                    out.push((
                        i,
                        format!(
                            "dropping model {model} destroys all of its data and any rows that \
                             reference it (cascading); this is irreversible"
                        ),
                    ));
                }
                MigrationOp::DropField { model, field } => {
                    out.push((
                        i,
                        format!(
                            "dropping {model}.{field} destroys that column's data and cannot be \
                             rolled back by the down migration"
                        ),
                    ));
                }
                MigrationOp::CreateModel { .. } => {}
            }
        }
        out
    }
}

/// Human-readable engine name for an advisory.
fn target_dialect_name(target: Target) -> &'static str {
    match target {
        Target::SeaOrmPostgres => "Postgres",
        Target::SeaOrmMysql => "MySQL",
        Target::Mongo => "MongoDB",
        Target::SeaOrmSqlite => "SQLite",
        _ => "the target database",
    }
}

fn diff_fields(from: &LockModel, to: &LockModel, ops: &mut Vec<MigrationOp>) {
    let from_fields: BTreeMap<&str, &LockField> =
        from.fields.iter().map(|f| (f.name.as_str(), f)).collect();
    let to_fields: BTreeMap<&str, &LockField> =
        to.fields.iter().map(|f| (f.name.as_str(), f)).collect();

    for (name, tf) in &to_fields {
        let uniq = tf.intents.iter().any(|i| i == "-uniq");
        let nullable = tf.intents.iter().any(|i| i == "-null");
        match from_fields.get(name) {
            None => ops.push(MigrationOp::AddField {
                model: to.name.clone(),
                field: (*name).to_string(),
                ty: tf.ty.clone(),
                nullable,
                uniq,
            }),
            Some(ff) => {
                let f_uniq = ff.intents.iter().any(|i| i == "-uniq");
                let f_null = ff.intents.iter().any(|i| i == "-null");
                if ff.ty != tf.ty || f_uniq != uniq || f_null != nullable {
                    ops.push(MigrationOp::AlterField {
                        model: to.name.clone(),
                        field: (*name).to_string(),
                        new_ty: tf.ty.clone(),
                        nullable,
                        uniq,
                    });
                }
            }
        }
    }

    for name in from_fields.keys() {
        if !to_fields.contains_key(name) {
            ops.push(MigrationOp::DropField {
                model: to.name.clone(),
                field: (*name).to_string(),
            });
        }
    }
}

/// Convenience: diff a validated target AST against a current lockfile.
pub fn plan_migration(current: &Lockfile, target_ast: &Ast) -> Result<MigrationPlan, OsdlError> {
    let target = Lockfile::from_ast(target_ast);
    Ok(MigrationPlan::diff(current, &target))
}

/// Read a lockfile from a path (or `None` if it does not exist yet).
pub fn read_lockfile(path: &std::path::Path) -> Result<Option<Lockfile>, OsdlError> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)?;
    Ok(Some(Lockfile::from_str(&text)?))
}

/// Write a lockfile to a path.
pub fn write_lockfile(path: &std::path::Path, lf: &Lockfile) -> Result<(), OsdlError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, lf.to_string_pretty()?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use osdl_core::Target;
    use osdl_core::lockfile::lock_field;

    fn lf(models: Vec<LockModel>) -> Lockfile {
        Lockfile {
            version: Lockfile::VERSION,
            checksum: String::new(),
            models,
        }
    }

    #[test]
    fn detects_added_model_and_field() {
        let from = lf(vec![]);
        let to = lf(vec![LockModel {
            name: "User".into(),
            fields: vec![
                lock_field("id", "uuid", &["-pk"]),
                lock_field("email", "string", &["-uniq"]),
            ],
            indexes: vec![],
        }]);
        let plan = MigrationPlan::diff(&from, &to);
        assert_eq!(plan.ops.len(), 1);
        assert_eq!(
            plan.ops[0],
            MigrationOp::CreateModel {
                model: "User".into()
            }
        );
    }

    #[test]
    fn detects_field_changes() {
        let from = lf(vec![LockModel {
            name: "User".into(),
            fields: vec![
                lock_field("id", "uuid", &["-pk"]),
                lock_field("age", "int", &[]),
            ],
            indexes: vec![],
        }]);
        let to = lf(vec![LockModel {
            name: "User".into(),
            fields: vec![
                lock_field("id", "uuid", &["-pk"]),
                lock_field("age", "bigint", &["-null"]),
            ],
            indexes: vec![],
        }]);
        let plan = MigrationPlan::diff(&from, &to);
        assert_eq!(
            plan.ops[0],
            MigrationOp::AlterField {
                model: "User".into(),
                field: "age".into(),
                new_ty: "bigint".into(),
                nullable: true,
                uniq: false,
            }
        );
    }

    #[test]
    fn drops_come_last() {
        let from = lf(vec![LockModel {
            name: "Old".into(),
            fields: vec![],
            indexes: vec![],
        }]);
        let to = lf(vec![]);
        let plan = MigrationPlan::diff(&from, &to);
        assert_eq!(
            plan.ops,
            vec![MigrationOp::DropModel {
                model: "Old".into()
            }]
        );
    }

    #[test]
    fn destructive_detection() {
        let from = lf(vec![LockModel {
            name: "User".into(),
            fields: vec![
                lock_field("id", "uuid", &["-pk"]),
                lock_field("age", "int", &[]),
                lock_field("nick", "string", &[]),
            ],
            indexes: vec![],
        }]);
        let to = lf(vec![LockModel {
            name: "User".into(),
            fields: vec![
                lock_field("id", "uuid", &["-pk"]),
                lock_field("age", "bigint", &["-null"]),
            ],
            indexes: vec![],
        }]);
        let plan = MigrationPlan::diff(&from, &to);
        // drop field nick + alter field age are destructive; add field is not.
        assert!(plan.is_destructive());
        assert_eq!(plan.destructive_ops().len(), 2);
        let kinds: Vec<&str> = plan
            .destructive_ops()
            .iter()
            .map(|op| match op {
                MigrationOp::DropField { .. } => "drop",
                MigrationOp::AlterField { .. } => "alter",
                _ => "other",
            })
            .collect();
        assert!(kinds.contains(&"drop"));
        assert!(kinds.contains(&"alter"));
    }

    #[test]
    fn non_destructive_when_only_adds() {
        let from = lf(vec![]);
        let to = lf(vec![LockModel {
            name: "User".into(),
            fields: vec![lock_field("id", "uuid", &["-pk"])],
            indexes: vec![],
        }]);
        let plan = MigrationPlan::diff(&from, &to);
        assert!(!plan.is_destructive());
        assert!(plan.destructive_ops().is_empty());
    }

    #[test]
    fn integration_with_ast() {
        use osdl_parser::parse;
        let ast = parse("User\n  id uuid -pk\n  email string -uniq\n").unwrap();
        osdl_core::Validator::validate(&ast, Some(Target::SeaOrmSqlite)).unwrap();
        let current = lf(vec![]);
        let plan = plan_migration(&current, &ast).unwrap();
        assert_eq!(plan.ops.len(), 1);
        assert!(matches!(plan.ops[0], MigrationOp::CreateModel { .. }));
    }
}
