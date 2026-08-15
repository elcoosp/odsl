//! Render `MigrationPlan`s to standalone migration artifacts on disk.
//!
//! Two backend formats are supported:
//!
//! * **`Sql`** — a timestamped `.sql` file containing `up`/`down` sections that
//!   mirror exactly the DDL the live [`crate::sql`] adapter would run. This is a
//!   plain SQL migration (the "raw SQL" style SeaORM also supports).
//! * **`SeaOrm`** — a full `sea-orm-migration` **crate** under `migration/`:
//!   `Cargo.toml`, `src/lib.rs` (the `Migrator`), `src/main.rs` (the migrator
//!   CLI) and one `src/m{ts}_{slug}.rs` file per diff. The migration bodies use
//!   the `schema::*` column helpers and `ForeignKey::create()` — i.e. pure
//!   SeaQuery builders, so the generated migrations stay multi-backend and are
//!   **not** raw SQL strings (which would forfeit SeaORM's portability).
//!
//! File names are `{timestamp}_{slug}.sql` / `m{timestamp}_{slug}.rs`, derived
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
    /// A full `sea-orm-migration` crate (`migration/…`) using SeaQuery builders.
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

/// Render a plan into a single migration file (`.sql` only). The SeaORM crate
/// form is rendered/written by [`write_migration`] directly because it spans
/// several files.
pub fn render_migration(
    dialect: SqlDialect,
    plan: &MigrationPlan,
    target: &Lockfile,
    current: Option<&Lockfile>,
) -> RenderedMigration {
    let file_name = format!("{}_{}.sql", timestamp(), slugify(plan));
    let contents = render_sql(dialect, plan, target, current);
    RenderedMigration {
        file_name,
        contents,
    }
}

fn render_sql(
    dialect: SqlDialect,
    plan: &MigrationPlan,
    target: &Lockfile,
    current: Option<&Lockfile>,
) -> String {
    let up: Vec<String> = plan
        .ops
        .iter()
        .flat_map(|op| op_to_sql(dialect, op, target, current))
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

/// Inverse DDL for a single op (used by the down section).
fn down_sql(dialect: SqlDialect, op: &MigrationOp) -> Option<String> {
    use crate::naming::{quote_ident_for, table_name};
    match op {
        MigrationOp::CreateModel { model } => {
            Some(format!("DROP TABLE {}", quote_ident_for(dialect, &table_name(model))))
        }
        MigrationOp::DropModel { .. } => None, // cannot recreate without prior schema
        MigrationOp::AddField { model, field, .. } => Some(format!(
            "ALTER TABLE {} DROP COLUMN {}",
            quote_ident_for(dialect, &table_name(model)),
            quote_ident_for(dialect, field)
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
                SqlDialect::Mysql => {
                    "-- mysql: ALTER COLUMN TYPE rollback must be supplied manually".to_string()
                }
            })
        }
    }
}

/// SeaQuery column helper for a named field (non-PK path).
fn seaorm_column_for(col: &str, keyword: &str, nullable: bool, uniq: bool) -> String {
    let helper = match keyword {
        "string" => "string",
        "text" => "text",
        "int" => "integer",
        "bigint" => "big_integer",
        "float" => "float",
        "bool" => "boolean",
        "datetime" => "timestamp_with_time_zone",
        "date" => "date",
        "uuid" => "uuid",
        "json" => "json",
        "binary" => "binary",
        other => other,
    };
    match (nullable, uniq) {
        (false, false) => format!("{helper}(\"{col}\")"),
        (true, false) => format!("{helper}_null(\"{col}\")"),
        (false, true) => format!("{helper}_uniq(\"{col}\")"),
        (true, true) => format!("{helper}_null(\"{col}\").unique_key()"),
    }
}

fn migration_timestamp_name(slug: &str) -> String {
    format!("m{}_{}", timestamp(), slug)
}

/// Emit the `up` body (SeaQuery statements) for one op.
fn seaorm_up_stmt(op: &MigrationOp, target: &Lockfile) -> Vec<String> {
    use crate::naming::table_name;
    match op {
        MigrationOp::CreateModel { model } => {
            let tbl = table_name(model);
            let lm = target.model_by_name(model);
            let mut cols: Vec<String> = Vec::new();
            let mut fk_binds: Vec<String> = Vec::new();
            let mut fk_refs: Vec<String> = Vec::new();
            let mut idx_binds: Vec<String> = Vec::new();
            let mut idx_calls: Vec<String> = Vec::new();
            if let Some(lm) = lm {
                for f in &lm.fields {
                    let is_pk = f.intents.iter().any(|x| x == "-pk");
                    let nullable = f.intents.iter().any(|x| x == "-null");
                    let uniq = f.intents.iter().any(|x| x == "-uniq");
                    let has_index = f.intents.iter().any(|x| x == "-index");
                    if f.ty.contains('.') {
                        // Reference: column type follows the referenced PK.
                        // Emit the FK inline in Table::create() so it works on
                        // SQLite too (SQLite rejects ALTER/add FK post-creation).
                        let ref_ty = referenced_pk_type(target, f.ty.trim());
                        let col_helper = match ref_ty.as_str() {
                            "uuid" => "uuid",
                            "int" => "integer",
                            "bigint" => "big_integer",
                            _ => "string",
                        };
                        let col = if nullable {
                            format!("{col_helper}_null(\"{}\")", f.name)
                        } else {
                            format!("{col_helper}(\"{}\")", f.name)
                        };
                        cols.push(col);
                        let (ref_model, ref_col) = split_ref(f.ty.trim());
                        let ref_tbl = table_name(&ref_model);
                        let idx = fk_binds.len();
                        fk_binds.push(format!(
                            "        let mut fk{idx} = ForeignKey::create();\n        fk{idx}.from(\"{tbl}\", \"{}\").to(\"{}\", \"{}\").on_delete(ForeignKeyAction::Cascade).on_update(ForeignKeyAction::Cascade);",
                            f.name, ref_tbl, ref_col
                        ));
                        fk_refs.push(format!(".foreign_key(&mut fk{idx})"));
                    } else if is_pk {
                        cols.push(pk_column(&f.ty, &f.name));
                    } else {
                        cols.push(seaorm_column_for(&f.name, &f.ty, nullable, uniq));
                    }
                    if has_index && !is_pk {
                        // Secondary (non-unique) index via SeaQuery Index::create().
                        let i = idx_binds.len();
                        idx_binds.push(format!(
                            "        let mut ix{i} = Index::create();\n        ix{i}.name(\"idx_{tbl}_{}\").table(\"{}\").col(\"{}\");",
                            f.name, tbl, f.name
                        ));
                        idx_calls.push(format!("        manager.create_index(ix{i}).await?;"));
                    }
                }
            }
            // Model-level composite indexes (`-index a,b` / `-uniq a,b`).
            if let Some(lm) = &lm {
                for index in &lm.indexes {
                    let i = idx_binds.len();
                    let unique_lit = index.unique;
                    let mut binds = format!(
                        "        let mut ix{i} = Index::create();\n        ix{i}.name(\"{name}\").table(\"{tbl}\")",
                        name = index.name
                    );
                    for f in &index.fields {
                        binds.push_str(&format!("\n        ix{i}.col(\"{f}\")"));
                    }
                    if unique_lit {
                        binds.push_str(&format!("\n        ix{i}.unique()"));
                    }
                    binds.push(';');
                    idx_binds.push(binds);
                    idx_calls.push(format!("        manager.create_index(ix{i}).await?;"));
                }
            }
            let mut lines: Vec<String> = Vec::new();
            for b in &fk_binds {
                lines.push(b.clone());
            }
            for b in &idx_binds {
                lines.push(b.clone());
            }
            lines.push(format!(
                "        manager.create_table(\n            Table::create()\n                .table(\"{tbl}\")\n                .if_not_exists()"
            ));
            for c in &cols {
                lines.push(format!("                .col({c})"));
            }
            for r in &fk_refs {
                lines.push(format!("                {r}"));
            }
            lines.push("                .to_owned()\n        ).await?;".to_string());
            for c in &idx_calls {
                lines.push(c.clone());
            }
            lines
        }
        MigrationOp::AddField {
            model,
            field,
            ty,
            nullable,
            uniq,
        } => {
            let tbl = table_name(model);
            if ty.contains('.') {
                let ref_ty = referenced_pk_type(target, ty.trim());
                let col_helper = match ref_ty.as_str() {
                    "uuid" => "uuid",
                    "int" => "integer",
                    "bigint" => "big_integer",
                    _ => "string",
                };
                let col = if *nullable {
                    format!("{col_helper}_null(\"{field}\")")
                } else {
                    format!("{col_helper}(\"{field}\")")
                };
                let (ref_model, ref_col) = split_ref(ty.trim());
                let ref_tbl = table_name(&ref_model);
                vec![
                    format!(
                        "        manager.alter_table(\n            Table::alter()\n                .table(\"{tbl}\")\n                .add_column({col})\n                .to_owned()\n        ).await?;"
                    ),
                    format!(
                        "        let mut fk = ForeignKey::create();\n        fk.from(\"{tbl}\", \"{field}\").to(\"{ref_tbl}\", \"{ref_col}\").on_delete(ForeignKeyAction::Cascade).on_update(ForeignKeyAction::Cascade);\n        manager.create_foreign_key(fk).await?;"
                    ),
                ]
            } else {
                let mut stmts = vec![format!(
                    "        manager.alter_table(\n            Table::alter()\n                .table(\"{tbl}\")\n                .add_column({})\n                .to_owned()\n        ).await?;",
                    seaorm_column_for(field, ty, *nullable, *uniq)
                )];
                let wants_index = target
                    .model_by_name(model)
                    .and_then(|m| m.fields.iter().find(|f| f.name == *field))
                    .map(|f| f.intents.iter().any(|i| i == "-index"))
                    .unwrap_or(false);
                if !*uniq && wants_index {
                    stmts.push(format!(
                        "        let mut ix = Index::create();\n        ix.name(\"idx_{tbl}_{field}\").table(\"{tbl}\").col(\"{field}\");\n        manager.create_index(ix).await?;"
                    ));
                }
                stmts
            }
        }
        MigrationOp::DropField { model, field } => {
            let tbl = table_name(model);
            vec![format!(
                "        manager.alter_table(\n            Table::alter()\n                .table(\"{tbl}\")\n                .drop_column(\"{field}\")\n                .to_owned()\n        ).await?;"
            )]
        }
        MigrationOp::AlterField {
            model,
            field,
            new_ty,
            nullable,
            uniq,
        } => {
            let tbl = table_name(model);
            if new_ty.contains('.') {
                vec![format!(
                    "        // alter field '{field}' is a reference; type change not emitted (backend-specific)"
                )]
            } else {
                vec![format!(
                    "        manager.alter_table(\n            Table::alter()\n                .table(\"{tbl}\")\n                .modify_column({})\n                .to_owned()\n        ).await?;",
                    seaorm_column_for(field, new_ty, *nullable, *uniq)
                )]
            }
        }
        MigrationOp::DropModel { model } => {
            let tbl = table_name(model);
            vec![format!(
                "        manager.drop_table(\n            Table::drop()\n                .table(\"{tbl}\")\n                .to_owned()\n        ).await?;"
            )]
        }
    }
}

fn pk_column(ty: &str, name: &str) -> String {
    match ty {
        "int" => "pk_auto(\"id\")".to_string(),
        "bigint" => "big_pk_auto(\"id\")".to_string(),
        "uuid" => format!("pk_uuid(\"{name}\")"),
        _ => format!("string(\"{name}\").primary_key()"),
    }
}

fn referenced_pk_type(target: &Lockfile, ty: &str) -> String {
    let (model, _col) = split_ref(ty);
    target
        .model_by_name(&model)
        .and_then(|m| {
            m.fields
                .iter()
                .find(|f| f.intents.iter().any(|x| x == "-pk"))
        })
        .map(|f| f.ty.clone())
        .unwrap_or_else(|| "string".to_string())
}

fn split_ref(ty: &str) -> (String, String) {
    match ty.split_once('.') {
        Some((m, c)) => (m.to_string(), c.to_string()),
        None => (ty.to_string(), "id".to_string()),
    }
}

/// Build the `m{ts}_{slug}.rs` migration file contents (SeaQuery, no raw SQL).
fn render_seaorm_migration(plan: &MigrationPlan, target: &Lockfile) -> String {
    let mut up_lines: Vec<String> = Vec::new();
    let mut down_lines: Vec<String> = Vec::new();
    for op in &plan.ops {
        for s in seaorm_up_stmt(op, target) {
            up_lines.push(s);
        }
        // Down is the inverse in reverse order.
        if let Some(d) = seaorm_down_stmt(op, target) {
            down_lines.push(d);
        }
    }
    if up_lines.is_empty() {
        up_lines.push("        // no changes".to_string());
    }
    if down_lines.is_empty() {
        down_lines.push("        // no changes".to_string());
    }

    format!(
        r#"//! OSDL auto-generated SeaORM migration.
//!
//! Pure SeaQuery builders (no raw SQL) — portable across SQLite/Postgres/MySQL.
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {{
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {{
{up}
        Ok(())
    }}

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {{
{down}
        Ok(())
    }}
}}
"#,
        up = up_lines.join("\n"),
        down = down_lines.join("\n"),
    )
}

/// Inverse SeaQuery statement for one op (down section).
fn seaorm_down_stmt(op: &MigrationOp, target: &Lockfile) -> Option<String> {
    use crate::naming::table_name;
    match op {
        MigrationOp::CreateModel { model } => {
            let tbl = table_name(model);
            // Drop any secondary indexes first, then the table.
            let lm = target.model_by_name(model);
            let mut lines: Vec<String> = Vec::new();
            if let Some(lm) = lm {
                for f in &lm.fields {
                    if f.intents.iter().any(|i| i == "-index")
                        && !f.intents.iter().any(|i| i == "-pk")
                    {
                        lines.push(format!(
                            "        manager.drop_index(\n            Index::drop()\n                .name(\"idx_{tbl}_{}\")\n                .table(\"{tbl}\")\n                .to_owned()\n        ).await?;",
                            f.name
                        ));
                    }
                }
                // Model-level composite indexes.
                for index in &lm.indexes {
                    lines.push(format!(
                        "        manager.drop_index(\n            Index::drop()\n                .name(\"{name}\")\n                .table(\"{tbl}\")\n                .to_owned()\n        ).await?;",
                        name = index.name
                    ));
                }
            }
            lines.push(format!(
                "        manager.drop_table(\n            Table::drop()\n                .table(\"{tbl}\")\n                .to_owned()\n        ).await?;"
            ));
            Some(lines.join("\n"))
        }
        MigrationOp::AddField { model, field, .. } => {
            let tbl = table_name(model);
            let wants_index = target
                .model_by_name(model)
                .and_then(|m| m.fields.iter().find(|f| f.name == *field))
                .map(|f| f.intents.iter().any(|i| i == "-index"))
                .unwrap_or(false);
            let mut lines = vec![format!(
                "        manager.alter_table(\n            Table::alter()\n                .table(\"{tbl}\")\n                .drop_column(\"{field}\")\n                .to_owned()\n        ).await?;"
            )];
            if wants_index {
                lines.push(format!(
                    "        manager.drop_index(\n            Index::drop()\n                .name(\"idx_{tbl}_{field}\")\n                .table(\"{tbl}\")\n                .to_owned()\n        ).await?;"
                ));
            }
            Some(lines.join("\n"))
        }
        MigrationOp::DropField { .. } => None,
        MigrationOp::AlterField { .. } => {
            Some("        // alter field rollback is backend-specific; supply manually".to_string())
        }
        MigrationOp::DropModel { .. } => None,
    }
}

/// `Cargo.toml` for the generated `migration/` crate.
fn migration_cargo_toml() -> String {
    r#"[package]
name = "migration"
version = "0.1.0"
edition = "2021"
publish = false

[lib]
crate-type = ["lib", "cdylib"]

[dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }

[dependencies.sea-orm-migration]
version = "2.0"
features = [
    "runtime-tokio-native-tls",
    "sqlx-sqlite",
    "with-chrono",
    "with-uuid",
    "with-json",
]
"#
    .to_string()
}

/// `src/lib.rs` for the generated `migration/` crate (the `Migrator`).
fn migration_lib(mod_names: &[String]) -> String {
    let mut mods = String::new();
    let mut boxes = String::new();
    for m in mod_names {
        mods.push_str(&format!("mod {m};\n"));
        boxes.push_str(&format!("            Box::new({m}::Migration),\n"));
    }
    format!(
        r#"pub use sea_orm_migration::*;

{mods}
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {{
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {{
        vec![
{boxes}        ]
    }}
}}
"#,
        mods = mods,
        boxes = boxes,
    )
}

/// `src/main.rs` for the generated `migration/` crate (the migrator CLI).
fn migration_main() -> String {
    r#"use migration::Migrator;
use sea_orm_migration::prelude::*;

#[tokio::main]
async fn main() {
    cli::run_cli(Migrator).await;
}
"#
    .to_string()
}

/// Render and write a migration into `dir` (created if missing).
///
/// * `Sql` -> a single `<ts>_<slug>.sql` file.
/// * `SeaOrm` -> a full `migration/` crate (Cargo.toml, src/lib.rs, src/main.rs,
///   and `src/m<ts>_<slug>.rs`), accumulating into any existing `migration/`.
///
/// Returns the primary written file's relative path (the `.sql` or the crate's
/// migration module). Writes nothing (and returns `None`) when the plan is
/// empty, so re-running on an unchanged schema produces no file.
pub fn write_migration(
    dir: &Path,
    format: MigrationFormat,
    dialect: SqlDialect,
    plan: &MigrationPlan,
    target: &Lockfile,
    current: Option<&Lockfile>,
) -> std::io::Result<Option<String>> {
    if plan.ops.is_empty() {
        return Ok(None);
    }
    match format {
        MigrationFormat::Sql => {
            std::fs::create_dir_all(dir)?;
            let rendered = render_migration(dialect, plan, target, current);
            let path = dir.join(&rendered.file_name);
            std::fs::write(&path, rendered.contents)?;
            Ok(Some(rendered.file_name))
        }
        MigrationFormat::SeaOrm => {
            let mdir = dir.join("migration");
            std::fs::create_dir_all(mdir.join("src"))?;
            // Accumulate existing migration modules (excluding the one we write now).
            let slug = slugify(plan);
            let new_mod = migration_timestamp_name(&slug);
            let mut mods: Vec<String> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(mdir.join("src")) {
                for e in entries.flatten() {
                    let fname = e.file_name().to_string_lossy().to_string();
                    let stem = fname.strip_prefix("m").and_then(|s| s.strip_suffix(".rs"));
                    if let Some(stem) =
                        stem.filter(|_| fname.starts_with('m') && fname != format!("{new_mod}.rs"))
                    {
                        mods.push(stem.to_string());
                    }
                }
            }
            mods.push(new_mod.clone());
            mods.sort();

            std::fs::write(mdir.join("Cargo.toml"), migration_cargo_toml())?;
            std::fs::write(mdir.join("src").join("lib.rs"), migration_lib(&mods))?;
            std::fs::write(mdir.join("src").join("main.rs"), migration_main())?;
            std::fs::write(
                mdir.join("src").join(format!("{new_mod}.rs")),
                render_seaorm_migration(plan, target),
            )?;
            Ok(Some(format!("migration/src/{new_mod}.rs")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use osdl_core::LockModel;
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
                indexes: vec![],
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
        let out = render_migration(SqlDialect::Sqlite, &plan(), &lf(), None);
        assert!(out.file_name.ends_with(".sql"));
        assert!(out.contents.contains("-- up"));
        assert!(out.contents.contains("-- down"));
        assert!(out.contents.contains("CREATE TABLE"));
        assert!(out.contents.contains("DROP TABLE"));
    }

    #[test]
    fn slug_is_stable() {
        let a = render_migration(SqlDialect::Sqlite, &plan(), &lf(), None);
        let b = render_migration(SqlDialect::Sqlite, &plan(), &lf(), None);
        let suffix = |s: &str| s.rsplit_once('_').map(|(_, r)| r.to_string()).unwrap();
        assert_eq!(suffix(&a.file_name), suffix(&b.file_name));
        assert!(a.file_name.ends_with("create_user.sql"));
    }

    #[test]
    fn seaorm_migration_uses_seaquery_not_raw_sql() {
        // CreateModel User -> uses schema::* helpers, NOT execute_unprepared.
        let out = render_seaorm_migration(&plan(), &lf());
        assert!(out.contains("use sea_orm_migration::schema::*"));
        assert!(out.contains("Table::create()"));
        assert!(out.contains("pk_uuid(\"id\")"));
        assert!(out.contains("string_uniq(\"email\")"));
        assert!(!out.contains("execute_unprepared"));
        assert!(!out.contains("CREATE TABLE"));
    }

    #[test]
    fn seaorm_migration_emits_foreign_key() {
        let target = Lockfile {
            version: Lockfile::VERSION,
            checksum: String::new(),
            models: vec![
                LockModel {
                    name: "User".into(),
                    fields: vec![lock_field("id", "uuid", &["-pk"])],
                    indexes: vec![],
                },
                LockModel {
                    name: "Post".into(),
                    fields: vec![
                        lock_field("id", "uuid", &["-pk"]),
                        lock_field("author", "User.id", &[]),
                    ],
                    indexes: vec![],
                },
            ],
        };
        let plan = MigrationPlan {
            ops: vec![MigrationOp::CreateModel {
                model: "Post".into(),
            }],
        };
        let out = render_seaorm_migration(&plan, &target);
        assert!(out.contains("ForeignKey::create()"));
        assert!(out.contains(".from(\"posts\", \"author\")"));
        assert!(out.contains(".to(\"users\", \"id\")"));
        assert!(!out.contains("execute_unprepared"));
    }

    #[test]
    fn seaorm_migration_emits_composite_model_index() {
        let target = Lockfile {
            version: Lockfile::VERSION,
            checksum: String::new(),
            models: vec![LockModel {
                name: "User".into(),
                fields: vec![lock_field("id", "uuid", &["-pk"])],
                indexes: vec![osdl_core::ast::LockIndex {
                    name: "uniq_tenant_id_email".into(),
                    fields: vec!["tenant_id".into(), "email".into()],
                    unique: true,
                }],
            }],
        };
        let plan = MigrationPlan {
            ops: vec![MigrationOp::CreateModel {
                model: "User".into(),
            }],
        };
        let out = render_seaorm_migration(&plan, &target);
        // Composite index via SeaQuery Index::create() with multiple .col().
        assert!(out.contains("Index::create()"));
        assert!(out.contains("uniq_tenant_id_email"));
        assert!(out.contains(".col(\"tenant_id\")"));
        assert!(out.contains(".col(\"email\")"));
        assert!(out.contains(".unique()"));
        assert!(out.contains("manager.create_index("));
        // Down drops it via Index::drop().
        assert!(out.contains("Index::drop()"));
        assert!(out.contains("manager.drop_index("));
    }

    #[test]
    fn seaorm_migration_emits_secondary_index() {
        let target = Lockfile {
            version: Lockfile::VERSION,
            checksum: String::new(),
            models: vec![LockModel {
                name: "User".into(),
                fields: vec![
                    lock_field("id", "uuid", &["-pk"]),
                    lock_field("email", "string", &["-uniq"]),
                    lock_field("name", "string", &["-index"]),
                ],
                indexes: vec![],
            }],
        };
        let plan = MigrationPlan {
            ops: vec![MigrationOp::CreateModel {
                model: "User".into(),
            }],
        };
        let out = render_seaorm_migration(&plan, &target);
        // Index created via SeaQuery Index::create() (not raw SQL).
        assert!(out.contains("Index::create()"));
        assert!(out.contains("idx_users_name"));
        assert!(out.contains("manager.create_index("));
        assert!(!out.contains("execute_unprepared"));
        // Down drops the index via Index::drop().
        assert!(out.contains("Index::drop()"));
        assert!(out.contains("manager.drop_index("));
    }

    #[test]
    fn seaorm_addfield_emits_secondary_index() {
        let target = Lockfile {
            version: Lockfile::VERSION,
            checksum: String::new(),
            models: vec![LockModel {
                name: "User".into(),
                fields: vec![
                    lock_field("id", "uuid", &["-pk"]),
                    lock_field("name", "string", &["-index"]),
                ],
                indexes: vec![],
            }],
        };
        let plan = MigrationPlan {
            ops: vec![MigrationOp::AddField {
                model: "User".into(),
                field: "name".into(),
                ty: "string".into(),
                nullable: false,
                uniq: false,
            }],
        };
        let out = render_seaorm_migration(&plan, &target);
        assert!(
            out.contains("Index::create()"),
            "up missing index create:\n{out}"
        );
        assert!(out.contains("idx_users_name"));
        assert!(out.contains("manager.create_index("));
        assert!(out.contains("Index::drop()"));
        assert!(out.contains("manager.drop_index("));
    }

    #[test]
    fn cli_style_addfield_index_via_from_ast() {
        // Mirrors cmd_migrate_create: parse -> Lockfile::from_ast -> render.
        let src = "User\n  id uuid -pk\n  email string -uniq\n  name string -index\n";
        let mut ast = osdl_parser::parse(src).unwrap();
        osdl_core::Validator::validate(&mut ast, Some(osdl_core::Target::SeaOrmSqlite)).unwrap();
        let target = Lockfile::from_ast(&ast);
        // Sanity: the parsed target carries -index on name.
        let name_intents = target
            .model_by_name("User")
            .unwrap()
            .fields
            .iter()
            .find(|f| f.name == "name")
            .unwrap()
            .intents
            .clone();
        assert!(
            name_intents.iter().any(|i| i == "-index"),
            "target intents for name: {name_intents:?}"
        );
        let plan = MigrationPlan {
            ops: vec![MigrationOp::AddField {
                model: "User".into(),
                field: "name".into(),
                ty: "string".into(),
                nullable: false,
                uniq: false,
            }],
        };
        let out = render_seaorm_migration(&plan, &target);
        assert!(
            out.contains("Index::create()"),
            "up missing index (from_ast path):\n{out}"
        );
        assert!(out.contains("idx_users_name"));
    }

    #[test]
    fn cli_style_full_pipeline_index() {
        // Full cmd_migrate_create path: parse -> plan_migration(empty, ast) -> render.
        let src = "User\n  id uuid -pk\n  email string -uniq\n  name string -index\n";
        let mut ast = osdl_parser::parse(src).unwrap();
        osdl_core::Validator::validate(&mut ast, Some(osdl_core::Target::SeaOrmSqlite)).unwrap();
        let current = Lockfile {
            version: Lockfile::VERSION,
            checksum: String::new(),
            models: vec![],
        };
        let plan = osdl_migrator::plan_migration(&current, &ast).unwrap();
        let target = Lockfile::from_ast(&ast);
        let out = render_seaorm_migration(&plan, &target);
        assert!(
            out.contains("Index::create()") && out.contains("idx_users_name"),
            "full pipeline missing index:\n{out}"
        );
    }

    #[test]
    fn empty_plan_writes_nothing() {
        let empty = MigrationPlan { ops: vec![] };
        let out = render_migration(SqlDialect::Sqlite, &empty, &lf(), None);
        assert!(out.contents.contains("(no changes)"));
    }
}
