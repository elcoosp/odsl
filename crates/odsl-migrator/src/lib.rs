//! ODSL migration engine.
//!
//! The migrator compares two schema snapshots — a *current* [`Lockfile`] (what
//! is deployed, from `odsl.lock`) and a *target* [`Ast`] (the new `.odsl` after
//! validation) — and produces a deterministic, ordered list of
//! [`MigrationOp`]s. Each op carries enough metadata for a backend adapter
//! (SeaORM `sea-orm-migration`, MongoDB `$jsonschema` validators) to apply it.
//!
//! Determinism: ops are emitted in a stable order (drops last, creates first)
//! so the same schema delta always yields the same migration plan.

#![allow(clippy::result_large_err)]

use odsl_core::Target;
use odsl_core::ast::{Ast, LockField, LockModel, LockView};
use odsl_core::errors::OdslError;
use odsl_core::lockfile::Lockfile;
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
    /// Create a view (read-model) that did not exist before.
    CreateView { view: String },
    /// Drop a view that no longer exists.
    DropView { view: String },
    /// Insert seed data into a model that was not previously seeded.
    /// Non-destructive: emitted only when the target seeds a model the current
    /// lockfile did not; changing seed *content* is not re-applied (data is
    /// left to the adapter's upsert semantics, and removing a seed never drops
    /// data).
    SeedData { model: String },
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

        // Views (read-models). Created/dropped by name; a changed view query or
        // projection is treated as drop + create (views are cheap to rebuild).
        let from_views: BTreeMap<&str, &LockView> =
            from.views.iter().map(|v| (v.name.as_str(), v)).collect();
        let to_views: BTreeMap<&str, &LockView> =
            to.views.iter().map(|v| (v.name.as_str(), v)).collect();
        let mut view_drops = Vec::new();
        for (name, tv) in &to_views {
            match from_views.get(name) {
                None => ops.push(MigrationOp::CreateView {
                    view: (*name).to_string(),
                }),
                Some(fv) if fv != tv => {
                    // Changed: drop the old, create the new.
                    view_drops.push(MigrationOp::DropView {
                        view: (*name).to_string(),
                    });
                    ops.push(MigrationOp::CreateView {
                        view: (*name).to_string(),
                    });
                }
                Some(_) => {}
            }
        }
        for name in from_views.keys() {
            if !to_views.contains_key(name) {
                view_drops.push(MigrationOp::DropView {
                    view: (*name).to_string(),
                });
            }
        }
        ops.extend(view_drops);

        // Seeds (fixtures). A seed is emitted as a `SeedData` op only when the
        // *target* seeds a model that the *current* lockfile did not (i.e. the
        // data is genuinely new). Changing seed content for an already-seeded
        // model is NOT re-applied — data is left to the adapter's upsert
        // semantics and schema diffs stay structural. Removing a seed never
        // drops data (no `DropSeed` op), so seeding is always non-destructive.
        let from_seeds: std::collections::HashSet<&str> =
            from.seeds.iter().map(|s| s.model.as_str()).collect();
        for seed in &to.seeds {
            if !from_seeds.contains(seed.model.as_str()) {
                ops.push(MigrationOp::SeedData {
                    model: seed.model.clone(),
                });
            }
        }

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
                MigrationOp::CreateView { view } => format!("create view {view}"),
                MigrationOp::DropView { view } => format!("drop view {view}"),
                MigrationOp::SeedData { model } => format!("seed data {model}"),
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
                MigrationOp::CreateModel { .. }
                | MigrationOp::AddField { .. }
                | MigrationOp::CreateView { .. }
                | MigrationOp::DropView { .. }
                | MigrationOp::SeedData { .. } => false,
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
                    if !*nullable && matches!(target, Target::SeaOrmPostgres | Target::SeaOrmMysql)
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
                // Views hold no data: creating/dropping them is online-safe and
                // non-destructive, so no advisory is raised.
                MigrationOp::CreateView { .. } | MigrationOp::DropView { .. } => {}
                // Seeds are non-destructive inserts; no advisory.
                MigrationOp::SeedData { .. } => {}
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
pub fn plan_migration(current: &Lockfile, target_ast: &Ast) -> Result<MigrationPlan, OdslError> {
    let target = Lockfile::from_ast(target_ast);
    Ok(MigrationPlan::diff(current, &target))
}

/// Read a lockfile from a path (or `None` if it does not exist yet).
pub fn read_lockfile(path: &std::path::Path) -> Result<Option<Lockfile>, OdslError> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)?;
    Ok(Some(Lockfile::from_str(&text)?))
}

/// Write a lockfile to a path.
pub fn write_lockfile(path: &std::path::Path, lf: &Lockfile) -> Result<(), OdslError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, lf.to_string_pretty()?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use odsl_core::Target;
    use odsl_core::lockfile::lock_field;

    fn lf(models: Vec<LockModel>) -> Lockfile {
        Lockfile {
            seeds: vec![],
            version: Lockfile::VERSION,
            checksum: String::new(),
            models,
            views: vec![],
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
            primary_key: vec![],
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
            primary_key: vec![],
        }]);
        let to = lf(vec![LockModel {
            name: "User".into(),
            fields: vec![
                lock_field("id", "uuid", &["-pk"]),
                lock_field("age", "bigint", &["-null"]),
            ],
            indexes: vec![],
            primary_key: vec![],
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
            primary_key: vec![],
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
            primary_key: vec![],
        }]);
        let to = lf(vec![LockModel {
            name: "User".into(),
            fields: vec![
                lock_field("id", "uuid", &["-pk"]),
                lock_field("age", "bigint", &["-null"]),
            ],
            indexes: vec![],
            primary_key: vec![],
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
            primary_key: vec![],
        }]);
        let plan = MigrationPlan::diff(&from, &to);
        assert!(!plan.is_destructive());
        assert!(plan.destructive_ops().is_empty());
    }

    #[test]
    fn integration_with_ast() {
        use odsl_parser::parse;
        let ast = parse("User\n  id uuid -pk\n  email string -uniq\n").unwrap();
        odsl_core::Validator::validate(&ast, Some(Target::SeaOrmSqlite)).unwrap();
        let current = lf(vec![]);
        let plan = plan_migration(&current, &ast).unwrap();
        assert_eq!(plan.ops.len(), 1);
        assert!(matches!(plan.ops[0], MigrationOp::CreateModel { .. }));
    }

    #[test]
    fn diff_detects_added_and_dropped_views() {
        let from = lf(vec![]);
        let to = lf(vec![]);
        let mut to_with_views = to;
        to_with_views.views = vec![
            LockView {
                name: "RecentPosts".into(),
                fields: vec![],
                query: "SELECT p.id FROM posts p".into(),
                materialized: false,
            },
            LockView {
                name: "ActiveUsers".into(),
                fields: vec![],
                query: "SELECT u.id FROM users u WHERE u.active = true".into(),
                materialized: true,
            },
        ];
        let plan = MigrationPlan::diff(&from, &to_with_views);
        // Two CREATE VIEW ops (order-independent).
        assert_eq!(plan.ops.len(), 2);
        let creates: Vec<&str> = plan
            .ops
            .iter()
            .filter_map(|op| match op {
                MigrationOp::CreateView { view } => Some(view.as_str()),
                _ => None,
            })
            .collect();
        assert!(creates.contains(&"RecentPosts"));
        assert!(creates.contains(&"ActiveUsers"));

        // Removing a view produces a DROP VIEW.
        let to_empty = lf(vec![]);
        let mut from_with_views = lf(vec![]);
        from_with_views.views = vec![LockView {
            name: "RecentPosts".into(),
            fields: vec![],
            query: "SELECT 1".into(),
            materialized: false,
        }];
        let plan2 = MigrationPlan::diff(&from_with_views, &to_empty);
        assert_eq!(plan2.ops.len(), 1);
        assert_eq!(
            plan2.ops[0],
            MigrationOp::DropView {
                view: "RecentPosts".into()
            }
        );
        // Views are non-destructive.
        assert!(!plan2.is_destructive());
    }

    #[test]
    fn detects_seed_data_for_newly_seeded_model() {
        use odsl_core::ast::LockSeed;
        let from = lf(vec![]);
        let mut to = lf(vec![LockModel {
            name: "User".into(),
            fields: vec![lock_field("id", "uuid", &["-pk"])],
            indexes: vec![],
            primary_key: vec![],
        }]);
        to.seeds = vec![LockSeed {
            model: "User".into(),
            rows: vec![vec![(
                "id".into(),
                "00000000-0000-0000-0000-000000000001".into(),
            )]],
        }];
        let plan = MigrationPlan::diff(&from, &to);
        // Only a SeedData op is emitted (no structural ops since the model
        // already existed in `from`? Here `from` is empty so CreateModel also
        // fires). The seed must be present and non-destructive.
        let seed_ops: Vec<&str> = plan
            .ops
            .iter()
            .filter_map(|op| match op {
                MigrationOp::SeedData { model } => Some(model.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(seed_ops, vec!["User"]);
        // Seeding is non-destructive.
        assert!(
            !plan
                .destructive_ops()
                .iter()
                .any(|op| matches!(op, MigrationOp::SeedData { .. }))
        );
    }

    #[test]
    fn seed_data_is_not_reapplied_for_already_seeded_model() {
        use odsl_core::ast::LockSeed;
        let mut from = lf(vec![LockModel {
            name: "User".into(),
            fields: vec![lock_field("id", "uuid", &["-pk"])],
            indexes: vec![],
            primary_key: vec![],
        }]);
        from.seeds = vec![LockSeed {
            model: "User".into(),
            rows: vec![vec![(
                "id".into(),
                "11111111-1111-1111-1111-111111111111".into(),
            )]],
        }];
        let to = from.clone();
        let plan = MigrationPlan::diff(&from, &to);
        // No SeedData op when the seed is unchanged.
        assert!(
            !plan
                .ops
                .iter()
                .any(|op| matches!(op, MigrationOp::SeedData { .. }))
        );
    }
}
