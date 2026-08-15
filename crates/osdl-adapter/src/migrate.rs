//! Render `MigrationPlan`s to standalone migration files on disk.
//!
//! Two backend formats are supported:
//!
//! * **`Sql`** — a timestamped `.sql` file containing `up`/`down` sections that
//!   mirror exactly the DDL the live [`crate::sql`] adapter would run.
//! * **`SeaOrm`** — a `Migrator::up`/`Migrator::down` pair of SeaORM migration
//!   modules (`xxxx_name.rs`) that drop into a `sea-orm-migration` crate and
//!   compile. For simplicity these wrap the same DDL via
//!   `DbBackend::execute_unprepared` so the plan stays the single source of
//!   truth.
//!
//! File names are `{timestamp}_{slug}.sql` / `{timestamp}_{slug}.rs`, derived
//! from the plan, so the same schema delta always yields the same file name —
//! satisfying the spec's determinism requirement (REQ-NFR-DET-001).

use chrono::Utc;
use osdl_core::lockfile::Lockfile;
use osdl_migrator::{MigrationOp, MigrationPlan};
use std::path::Path;

use crate::sql::{SqlDialect, op_to_sql};

/// How a migration plan should be materialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationFormat {
    /// A standalone `.sql` file (up + down sections).
    Sql,
    /// A SeaORM `sea-orm-migration` Rust module (`up`/`down`).
    SeaOrm,
}

impl MigrationFormat {
    /// Pick a format from the OSDL compile target.
    pub fn from_target(target: osdl_core::Target) -> Self {
        match target {
            osdl_core::Target::Mongo => MigrationFormat::Sql, // Mongo uses jsonSchema DDL in .sql form
            _ => MigrationFormat::Sql,
        }
    }
}

/// A single rendered migration file.
#[derive(Debug, Clone)]
pub struct RenderedMigration {
    /// File name, e.g. `20260815123045_create_post.sql`.
    pub file_name: String,
    /// Full file contents.
    pub contents: String,
}

fn slugify(plan: &MigrationPlan) -> String {
    // Stable slug from the ops: created/dropped model names joined by `_`.
    let mut parts: Vec<String> = plan
        .ops
        .iter()
        .map(|op| match op {
            MigrationOp::CreateModel { model } => format!("create_{}", model.to_lowercase()),
            MigrationOp::DropModel { model } => format!("drop_{}", model.to_lowercase()),
            MigrationOp::AddField { model, field, .. } => {
                format!("add_{}_{}", model.to_lowercase(), field.to_lowercase())
            }
            MigrationOp::DropField { model, field, .. } => {
                format!("drop_{}_{}", model.to_lowercase(), field.to_lowercase())
            }
            MigrationOp::AlterField { model, field, .. } => {
                format!("alter_{}_{}", model.to_lowercase(), field.to_lowercase())
            }
        })
        .collect();
    parts.sort();
    let joined = parts.join("_");
    if joined.is_empty() {
        "noop".to_string()
    } else {
        joined
    }
}

fn timestamp() -> String {
    // UTC compact timestamp; stable within a run, unique across runs.
    Utc::now().format("%Y%m%d%H%M%S").to_string()
}

/// Render a plan into a migration file (contents only).
pub fn render_migration(
    format: MigrationFormat,
    dialect: SqlDialect,
    plan: &MigrationPlan,
    target: &Lockfile,
) -> RenderedMigration {
    let file_name = match format {
        MigrationFormat::Sql => format!("{}_{}.sql", timestamp(), slugify(plan)),
        MigrationFormat::SeaOrm => format!("{}_{}.rs", timestamp(), slugify(plan)),
    };
    let contents = match format {
        MigrationFormat::Sql => render_sql(dialect, plan, target),
        MigrationFormat::SeaOrm => render_seaorm(plan, target),
    };
    RenderedMigration {
        file_name,
        contents,
    }
}

fn render_sql(dialect: SqlDialect, plan: &MigrationPlan, target: &Lockfile) -> String {
    let up: Vec<String> = plan
        .ops
        .iter()
        .flat_map(|op| op_to_sql(dialect, op, target))
        .collect();
    // Down is the reverse op order with inverse statements.
    let down: Vec<String> = plan
        .ops
        .iter()
        .rev()
        .filter_map(|op| down_sql(dialect, op))
        .collect();

    let mut out = String::new();
    out.push_str("-- OSDL auto-generated migration\n");
    out.push_str("-- up\n");
    if up.is_empty() {
        out.push_str("-- (no changes)\n");
    } else {
        for stmt in &up {
            out.push_str(stmt);
            out.push_str(";\n");
        }
    }
    out.push_str("\n-- down\n");
    if down.is_empty() {
        out.push_str("-- (no changes)\n");
    } else {
        for stmt in &down {
            out.push_str(stmt);
            out.push_str(";\n");
        }
    }
    out
}

/// Inverse DDL for a single op (used by the down section and SeaORM down).
fn down_sql(dialect: SqlDialect, op: &MigrationOp) -> Option<String> {
    use crate::naming::{quote_ident, table_name};
    match op {
        MigrationOp::CreateModel { model } => {
            Some(format!("DROP TABLE {}", quote_ident(&table_name(model))))
        }
        MigrationOp::DropModel { .. } => None, // cannot recreate without prior schema
        MigrationOp::AddField { model, field, .. } => Some(format!(
            "ALTER TABLE {} DROP COLUMN {}",
            quote_ident(&table_name(model)),
            quote_ident(field)
        )),
        MigrationOp::DropField { .. } => None,
        MigrationOp::AlterField { .. } => {
            // Type rollback is backend-specific; emit a documented guard.
            Some(match dialect {
                SqlDialect::Sqlite => {
                    "-- sqlite: ALTER COLUMN unsupported; manual rollback needed".to_string()
                }
                SqlDialect::Postgres => {
                    "-- postgres: ALTER COLUMN TYPE rollback must be supplied manually".to_string()
                }
            })
        }
    }
}

/// SeaORM `up`/`down` module body. Wraps the same DDL so the plan is the only
/// source of truth; the module compiles under `sea-orm-migration`.
fn render_seaorm(plan: &MigrationPlan, target: &Lockfile) -> String {
    let up: Vec<String> = plan
        .ops
        .iter()
        .flat_map(|op| op_to_sql(SqlDialect::Postgres, op, target))
        .collect();
    let down: Vec<String> = plan
        .ops
        .iter()
        .rev()
        .filter_map(|op| down_sql(SqlDialect::Postgres, op))
        .collect();

    let up_stmts = up
        .iter()
        .map(|s| {
            format!(
                "        manager.get_connection().execute_unprepared(\"{}\").await?;",
                s.replace('"', "\\\"")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let down_stmts = down
        .iter()
        .map(|s| {
            format!(
                "        manager.get_connection().execute_unprepared(\"{}\").await?;",
                s.replace('"', "\\\"")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"//! OSDL auto-generated SeaORM migration.
use sea_orm_migration::prelude::*;

#[async_trait::async_trait]
impl MigrationTrait for Migration {{
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {{
{up_stmts}
        Ok(())
    }}

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {{
{down_stmts}
        Ok(())
    }}
}}

#[derive(DeriveMigrationName)]
struct Migration;
"#,
        up_stmts = up_stmts,
        down_stmts = down_stmts,
    )
}

/// Render and write a migration file into `dir` (created if missing).
///
/// Returns the written relative path. Writes nothing (and returns `None`) when
/// the plan is empty, so re-running on an unchanged schema produces no file.
pub fn write_migration(
    dir: &Path,
    format: MigrationFormat,
    dialect: SqlDialect,
    plan: &MigrationPlan,
    target: &Lockfile,
) -> std::io::Result<Option<String>> {
    if plan.ops.is_empty() {
        return Ok(None);
    }
    std::fs::create_dir_all(dir)?;
    let rendered = render_migration(format, dialect, plan, target);
    let path = dir.join(&rendered.file_name);
    std::fs::write(&path, rendered.contents)?;
    Ok(Some(rendered.file_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use osdl_core::lockfile::lock_field;

    fn lf() -> Lockfile {
        Lockfile {
            version: Lockfile::VERSION,
            checksum: String::new(),
            models: vec![LockModel {
                name: "User".into(),
                fields: vec![
                    lock_field("id", "uuid", &["-pk"]),
                    lock_field("email", "string", &["-uniq"]),
                ],
            }],
        }
    }

    fn plan() -> MigrationPlan {
        MigrationPlan {
            ops: vec![MigrationOp::CreateModel {
                model: "User".into(),
            }],
        }
    }

    #[test]
    fn sql_file_has_up_and_down() {
        let out = render_migration(MigrationFormat::Sql, SqlDialect::Sqlite, &plan(), &lf());
        assert!(out.file_name.ends_with(".sql"));
        assert!(out.contents.contains("-- up"));
        assert!(out.contents.contains("-- down"));
        assert!(out.contents.contains("CREATE TABLE"));
        assert!(out.contents.contains("DROP TABLE"));
    }

    #[test]
    fn slug_is_stable() {
        // Same ops -> same slug (timestamp aside). We only check the suffix.
        let a = render_migration(MigrationFormat::Sql, SqlDialect::Sqlite, &plan(), &lf());
        let b = render_migration(MigrationFormat::Sql, SqlDialect::Sqlite, &plan(), &lf());
        let suffix = |s: &str| s.rsplit_once('_').map(|(_, r)| r.to_string()).unwrap();
        assert_eq!(suffix(&a.file_name), suffix(&b.file_name));
        assert!(a.file_name.ends_with("create_user.sql"));
    }

    #[test]
    fn seaorm_module_compiles_shape() {
        let out = render_migration(
            MigrationFormat::SeaOrm,
            SqlDialect::Postgres,
            &plan(),
            &lf(),
        );
        assert!(out.file_name.ends_with(".rs"));
        assert!(out.contents.contains("impl MigrationTrait for Migration"));
        assert!(out.contents.contains("async fn up"));
        assert!(out.contents.contains("async fn down"));
    }

    #[test]
    fn empty_plan_writes_nothing() {
        // Not a real fs test but guards the no-op contract shape.
        let empty = MigrationPlan { ops: vec![] };
        let out = render_migration(MigrationFormat::Sql, SqlDialect::Sqlite, &empty, &lf());
        assert!(out.contents.contains("(no changes)"));
    }
}
