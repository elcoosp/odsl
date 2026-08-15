//! SQL DDL generation for the SeaORM-backed adapters (SQLite & Postgres).
//!
//! OSDL scalar keywords map to backend-appropriate column types, and
//! `Model.field` references become foreign-key columns. All identifiers are
//! double-quoted (ANSI SQL) so reserved words and mixed case are safe.

use osdl_core::ast::LockModel;
use osdl_migrator::MigrationOp;

/// The SQL dialect the generator targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlDialect {
    Sqlite,
    Postgres,
    Mysql,
}

impl SqlDialect {
    /// Detect the dialect from a SeaORM connection URL scheme.
    pub fn from_url(url: &str) -> Option<Self> {
        if url.starts_with("sqlite:") {
            Some(SqlDialect::Sqlite)
        } else if url.starts_with("postgres:") || url.starts_with("postgresql:") {
            Some(SqlDialect::Postgres)
        } else if url.starts_with("mysql:") || url.starts_with("mariadb:") {
            Some(SqlDialect::Mysql)
        } else {
            None
        }
    }
}

/// Map an OSDL scalar keyword to a column type for the dialect.
pub fn column_type(dialect: SqlDialect, keyword: &str) -> &'static str {
    match (dialect, keyword) {
        (SqlDialect::Sqlite, "string") => "TEXT",
        (SqlDialect::Sqlite, "int") => "INTEGER",
        (SqlDialect::Sqlite, "bigint") => "BIGINT",
        (SqlDialect::Sqlite, "float") => "REAL",
        (SqlDialect::Sqlite, "bool") => "BOOLEAN",
        (SqlDialect::Sqlite, "datetime") => "TEXT",
        (SqlDialect::Sqlite, "date") => "TEXT",
        (SqlDialect::Sqlite, "uuid") => "TEXT",
        (SqlDialect::Sqlite, "json") => "TEXT",
        (SqlDialect::Sqlite, "binary") => "BLOB",
        (SqlDialect::Postgres, "string") => "TEXT",
        (SqlDialect::Postgres, "int") => "INTEGER",
        (SqlDialect::Postgres, "bigint") => "BIGINT",
        (SqlDialect::Postgres, "float") => "DOUBLE PRECISION",
        (SqlDialect::Postgres, "bool") => "BOOLEAN",
        (SqlDialect::Postgres, "datetime") => "TIMESTAMP",
        (SqlDialect::Postgres, "date") => "DATE",
        (SqlDialect::Postgres, "uuid") => "UUID",
        (SqlDialect::Postgres, "json") => "JSONB",
        (SqlDialect::Postgres, "binary") => "BYTEA",
        (SqlDialect::Mysql, "string") => "TEXT",
        (SqlDialect::Mysql, "int") => "INT",
        (SqlDialect::Mysql, "bigint") => "BIGINT",
        (SqlDialect::Mysql, "float") => "DOUBLE",
        (SqlDialect::Mysql, "bool") => "TINYINT(1)",
        (SqlDialect::Mysql, "datetime") => "DATETIME",
        (SqlDialect::Mysql, "date") => "DATE",
        (SqlDialect::Mysql, "uuid") => "CHAR(36)",
        (SqlDialect::Mysql, "json") => "JSON",
        (SqlDialect::Mysql, "binary") => "BLOB",
        // Unknown keyword -> safest portable type.
        (_, _) => "TEXT",
    }
}

/// An OSDL field's stored `ty` may be a reference (`Model.field`).
/// Returns the referenced (`model`, `field`) if so.
fn as_reference(ty: &str) -> Option<(&str, &str)> {
    let (head, tail) = ty.split_once('.')?;
    if tail.chars().all(|c| c.is_alphanumeric() || c == '_') && !head.is_empty() {
        Some((head, tail))
    } else {
        None
    }
}

/// Render a single column definition for `CREATE TABLE`.
fn column_def(dialect: SqlDialect, model: &LockModel, field: &osdl_core::ast::LockField) -> String {
    use crate::naming::*;
    let name = quote_ident_for(dialect, &field.name);
    let is_pk = field.intents.iter().any(|i| i == "-pk");
    let is_uniq = field.intents.iter().any(|i| i == "-uniq");
    let is_null = field.intents.iter().any(|i| i == "-null");
    let is_auto = field.intents.iter().any(|i| i == "-auto");

    let base_ty = if let Some((ref_model, ref_field)) = as_reference(&field.ty) {
        // Foreign key: use the referenced PK type (uuid by default) and add a
        // REFERENCES clause. OSDL references always target another model's id.
        format!(
            "{} REFERENCES {}({})",
            column_type(dialect, "uuid"),
            quote_ident_for(dialect, &table_name(ref_model)),
            quote_ident_for(dialect, ref_field)
        )
    } else {
        column_type(dialect, &field.ty).to_string()
    };

    let mut def = format!("{name} {base_ty}");
    if is_pk {
        def.push_str(" PRIMARY KEY");
        if is_auto && dialect == SqlDialect::Sqlite {
            def.push_str(" AUTOINCREMENT");
        }
    }
    if !is_null && !is_pk {
        def.push_str(" NOT NULL");
    }
    if is_uniq && !is_pk {
        def.push_str(" UNIQUE");
    }
    if let Some(expr) = &field.check_expr
        && !expr.trim().is_empty()
    {
        def.push_str(&format!(" CHECK ({})", expr.trim()));
    }
    let _ = model;
    def
}

/// Build the `CREATE TABLE` statement for a model from its lockfile projection.
pub fn create_table_sql(dialect: SqlDialect, model: &LockModel) -> String {
    use crate::naming::{quote_ident_for, table_name};
    let cols: Vec<String> = model
        .fields
        .iter()
        .map(|f| column_def(dialect, model, f))
        .collect();
    format!(
        "CREATE TABLE {} (\n  {}\n)",
        quote_ident_for(dialect, &table_name(&model.name)),
        cols.join(",\n  ")
    )
}

/// Translate one [`MigrationOp`] into the DDL statement(s) that apply it.
///
/// `target` supplies the full desired (post-migration) model projection.
/// `current` (when available) supplies the prior projection, which is required
/// to rebuild a SQLite table in place for operations SQLite cannot do with a
/// single `ALTER` (adding a NOT NULL / foreign-key column, dropping a column,
/// or changing a column's type).
pub fn op_to_sql(
    dialect: SqlDialect,
    op: &MigrationOp,
    target: &osdl_core::lockfile::Lockfile,
    current: Option<&osdl_core::lockfile::Lockfile>,
) -> Vec<String> {
    use crate::naming::{quote_ident_for, table_name};
    match op {
        MigrationOp::CreateModel { model } => {
            if let Some(m) = target.model_by_name(model) {
                vec![create_table_sql(dialect, m)]
            } else {
                // Defensive: target should always carry the model.
                vec![]
            }
        }
        MigrationOp::DropModel { model } => {
            vec![format!(
                "DROP TABLE {}",
                quote_ident_for(dialect, &table_name(model))
            )]
        }
        MigrationOp::AddField {
            model,
            field,
            ty,
            nullable,
            uniq,
        } => {
            // SQLite cannot add a NOT NULL column (on a non-empty table), a
            // foreign-key column, or a UNIQUE column in place — rebuild.
            let is_fk = as_reference(ty).is_some();
            if dialect == SqlDialect::Sqlite
                && (!nullable || is_fk || *uniq)
                && let Some(cur) = current
                && let Some(old) = cur.model_by_name(model)
                && let Some(new_lm) = target.model_by_name(model)
            {
                let mut fields = new_lm.fields.clone();
                // Ensure the new field is present even if `target` somehow lags.
                if !fields.iter().any(|f| f.name == *field) {
                    let col_ty = if is_fk { "uuid".into() } else { ty.clone() };
                    let mut intents = vec![];
                    if !nullable {
                        intents.push("-null".to_string());
                    }
                    if *uniq {
                        intents.push("-uniq".to_string());
                    }
                    fields.push(osdl_core::ast::LockField {
                        name: field.clone(),
                        ty: col_ty,
                        intents,
                        enum_variants: vec![],
                        default_value: None,
                        m2m_target: None,
                        check_expr: None,
                        polymorphic_targets: vec![],
                    });
                }
                return sqlite_rebuild_sql(old, model, &fields);
            }
            let col_ty = if is_fk {
                column_type(dialect, "uuid")
            } else {
                column_type(dialect, ty)
            };
            let mut def = format!(
                "ALTER TABLE {} ADD COLUMN {} {}",
                quote_ident_for(dialect, &table_name(model)),
                quote_ident_for(dialect, field),
                col_ty
            );
            if !nullable {
                def.push_str(" NOT NULL");
            }
            if *uniq {
                def.push_str(" UNIQUE");
            }
            vec![def]
        }
        MigrationOp::DropField { model, field } => {
            // SQLite < 3.35 lacks DROP COLUMN; rebuild to be safe.
            if dialect == SqlDialect::Sqlite
                && let Some(cur) = current
                && let Some(old) = cur.model_by_name(model)
                && let Some(new_lm) = target.model_by_name(model)
            {
                let kept: Vec<_> = new_lm
                    .fields
                    .iter()
                    .filter(|f| f.name != *field)
                    .cloned()
                    .collect();
                return sqlite_rebuild_sql(old, model, &kept);
            }
            vec![format!(
                "ALTER TABLE {} DROP COLUMN {}",
                quote_ident_for(dialect, &table_name(model)),
                quote_ident_for(dialect, field)
            )]
        }
        MigrationOp::AlterField {
            model,
            field,
            new_ty,
            nullable,
            uniq,
        } => {
            // SQLite cannot alter a column's type in place; Postgres/MySQL can.
            match dialect {
                SqlDialect::Postgres | SqlDialect::Mysql => {
                    let col_ty = if as_reference(new_ty).is_some() {
                        column_type(dialect, "uuid")
                    } else {
                        column_type(dialect, new_ty)
                    };
                    let def = format!(
                        "ALTER TABLE {} ALTER COLUMN {} TYPE {}",
                        quote_ident_for(dialect, &table_name(model)),
                        quote_ident_for(dialect, field),
                        col_ty
                    );
                    let _ = nullable;
                    let _ = uniq;
                    vec![def]
                }
                SqlDialect::Sqlite => {
                    // SQLite cannot ALTER COLUMN; rebuild the table preserving data.
                    if let Some(cur) = current
                        && let Some(old) = cur.model_by_name(model)
                        && let Some(new_lm) = target.model_by_name(model)
                    {
                        let mut fields = new_lm.fields.clone();
                        // Apply the type/nullable/uniq change to the target field.
                        if let Some(f) = fields.iter_mut().find(|f| f.name == *field) {
                            let is_fk = as_reference(new_ty).is_some();
                            f.ty = if is_fk { "uuid".into() } else { new_ty.clone() };
                            f.intents.retain(|i| i != "-null");
                            if !nullable && !f.intents.iter().any(|i| i == "-null") {
                                f.intents.push("-null".into());
                            }
                            if *uniq && !f.intents.iter().any(|i| i == "-uniq") {
                                f.intents.push("-uniq".into());
                            }
                        }
                        return sqlite_rebuild_sql(old, model, &fields);
                    }
                    // Best-effort: SQLite ignores the type but renaming to the
                    // same name is a no-op for type; emit a documented guard.
                    vec![format!(
                        "-- sqlite: ALTER COLUMN unsupported; manual migration needed for {}.{}",
                        table_name(model),
                        field
                    )]
                }
            }
        }
    }
}

/// SQLite has no in-place `ADD COLUMN ... NOT NULL` (on a non-empty table),
/// no `ADD COLUMN ... REFERENCES`, no `DROP COLUMN` on old versions, and no
/// `ALTER COLUMN`. The portable workaround is a full table rebuild:
///
/// 1. disable foreign keys
/// 2. create a shadow table `_osdl_new_<tbl>` with the desired columns
/// 3. copy rows across (only the columns shared by both schemas)
/// 4. drop the original table
/// 5. rename the shadow table into place
/// 6. re-enable foreign keys
///
/// This is OSDL's "12-step" SQLite rebuild, condensed to the essential,
/// data-preserving statements.
fn sqlite_rebuild_sql(
    old: &LockModel,
    new_model: &str,
    new_fields: &[osdl_core::ast::LockField],
) -> Vec<String> {
    use crate::naming::{quote_ident, table_name};
    let tbl = table_name(new_model);
    let shadow = format!("_osdl_new_{}", tbl);
    let new_lm = LockModel {
        name: new_model.to_string(),
        fields: new_fields.to_vec(),
        indexes: vec![],
    };
    let new_cols: Vec<String> = new_fields.iter().map(|f| quote_ident(&f.name)).collect();
    // For the INSERT, select old columns where they exist; for new columns
    // (present in the target but not the old schema) emit a type-appropriate
    // default literal so the NOT NULL constraint is satisfied.
    let select_exprs: Vec<String> = new_fields
        .iter()
        .map(|f| {
            if old.fields.iter().any(|of| of.name == f.name) {
                quote_ident(&f.name)
            } else {
                default_literal(f)
            }
        })
        .collect();

    vec![
        "PRAGMA foreign_keys=off;".to_string(),
        create_table_sql(SqlDialect::Sqlite, &new_lm).replace(
            &format!("CREATE TABLE {}", quote_ident(&tbl)),
            &format!("CREATE TABLE {}", quote_ident(&shadow)),
        ),
        format!(
            "INSERT INTO {} ({}) SELECT {} FROM {}",
            quote_ident(&shadow),
            new_cols.join(", "),
            select_exprs.join(", "),
            quote_ident(&tbl)
        ),
        format!("DROP TABLE {}", quote_ident(&tbl)),
        format!(
            "ALTER TABLE {} RENAME TO {}",
            quote_ident(&shadow),
            quote_ident(&tbl)
        ),
        "PRAGMA foreign_keys=on;".to_string(),
    ]
}

/// A type-appropriate DEFAULT literal for a freshly-added NOT NULL column so
/// the SQLite rebuild INSERT can satisfy the constraint.
fn default_literal(field: &osdl_core::ast::LockField) -> String {
    if as_reference(&field.ty).is_some() {
        "''".to_string()
    } else {
        match field.ty.as_str() {
            "int" | "bigint" | "bool" => "0".to_string(),
            "float" => "0.0".to_string(),
            "binary" => "X''".to_string(),
            _ => "''".to_string(), // string, uuid, date, datetime, json, unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use osdl_core::ast::LockModel;
    use osdl_core::lockfile::Lockfile;
    use osdl_core::lockfile::lock_field;

    fn user_model() -> LockModel {
        LockModel {
            name: "User".into(),
            fields: vec![
                lock_field("id", "uuid", &["-pk"]),
                lock_field("email", "string", &["-uniq"]),
            ],
            indexes: vec![],
        }
    }

    #[test]
    fn creates_sqlite_table() {
        let sql = create_table_sql(SqlDialect::Sqlite, &user_model());
        assert!(sql.contains("CREATE TABLE \"users\""));
        assert!(sql.contains("\"id\" TEXT PRIMARY KEY"));
        assert!(sql.contains("\"email\" TEXT"));
        assert!(sql.contains("UNIQUE"));
        assert!(sql.contains("NOT NULL"));
    }

    #[test]
    fn creates_postgres_table() {
        let sql = create_table_sql(SqlDialect::Postgres, &user_model());
        assert!(sql.contains("CREATE TABLE \"users\""));
        assert!(sql.contains("\"id\" UUID PRIMARY KEY"));
        assert!(sql.contains("\"email\" TEXT"));
        assert!(sql.contains("UNIQUE"));
    }

    #[test]
    fn creates_mysql_table_with_backticks() {
        let sql = create_table_sql(SqlDialect::Mysql, &user_model());
        // MySQL uses backtick quoting and CHAR(36) for uuid.
        assert!(sql.contains("CREATE TABLE `users`"));
        assert!(sql.contains("`id` CHAR(36) PRIMARY KEY"));
        assert!(sql.contains("`email` TEXT"));
        assert!(sql.contains("UNIQUE"));
        // SQLite/Postgres double-quote style must NOT appear for MySQL.
        assert!(!sql.contains("\"users\""));
    }

    #[test]
    fn mysql_column_types() {
        let m = LockModel {
            name: "Post".into(),
            fields: vec![
                lock_field("id", "uuid", &["-pk"]),
                lock_field("title", "string", &[]),
                lock_field("views", "bigint", &[]),
                lock_field("score", "float", &[]),
                lock_field("published", "bool", &[]),
                lock_field("created", "datetime", &[]),
            ],
            indexes: vec![],
        };
        let sql = create_table_sql(SqlDialect::Mysql, &m);
        assert!(sql.contains("CHAR(36)"));
        assert!(sql.contains("TEXT"));
        assert!(sql.contains("BIGINT"));
        assert!(sql.contains("DOUBLE"));
        assert!(sql.contains("TINYINT(1)"));
        assert!(sql.contains("DATETIME"));
    }

    #[test]
    fn mysql_add_field_is_inline() {
        let lf = osdl_core::lockfile::Lockfile {
            version: 1,
            checksum: String::new(),
            models: vec![user_model()],
        };
        let op = MigrationOp::AddField {
            model: "User".into(),
            field: "age".into(),
            ty: "int".into(),
            nullable: true,
            uniq: false,
        };
        let stmts = op_to_sql(SqlDialect::Mysql, &op, &lf, None);
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0], "ALTER TABLE `users` ADD COLUMN `age` INT");
    }

    #[test]
    fn mysql_alter_field_changes_type() {
        let lf = osdl_core::lockfile::Lockfile {
            version: 1,
            checksum: String::new(),
            models: vec![user_model()],
        };
        let op = MigrationOp::AlterField {
            model: "User".into(),
            field: "age".into(),
            new_ty: "bigint".into(),
            nullable: true,
            uniq: false,
        };
        let stmts = op_to_sql(SqlDialect::Mysql, &op, &lf, None);
        assert_eq!(
            stmts[0],
            "ALTER TABLE `users` ALTER COLUMN `age` TYPE BIGINT"
        );
    }

    #[test]
    fn reference_becomes_fk() {
        let m = LockModel {
            name: "Post".into(),
            fields: vec![
                lock_field("id", "uuid", &["-pk"]),
                lock_field("author", "User.id", &[]),
            ],
            indexes: vec![],
        };
        let sql = create_table_sql(SqlDialect::Postgres, &m);
        assert!(sql.contains("REFERENCES \"users\"(\"id\")"));
    }

    #[test]
    fn add_field_sql() {
        let lf = osdl_core::lockfile::Lockfile {
            version: 1,
            checksum: String::new(),
            models: vec![user_model()],
        };
        let op = MigrationOp::AddField {
            model: "User".into(),
            field: "age".into(),
            ty: "int".into(),
            nullable: true,
            uniq: false,
        };
        let stmts = op_to_sql(SqlDialect::Sqlite, &op, &lf, None);
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0], "ALTER TABLE \"users\" ADD COLUMN \"age\" INTEGER");
    }

    #[test]
    fn sqlite_add_not_null_rebuilds_table() {
        // Adding a NOT NULL column to a NON-empty table requires a full rebuild
        // on SQLite (no in-place ADD COLUMN ... NOT NULL). With `current`
        // present, op_to_sql must emit the shadow-table rebuild sequence.
        let current = Lockfile {
            version: 1,
            checksum: String::new(),
            models: vec![LockModel {
                name: "User".into(),
                fields: vec![lock_field("id", "uuid", &["-pk"])],
                indexes: vec![],
            }],
        };
        let target = Lockfile {
            version: 1,
            checksum: String::new(),
            models: vec![LockModel {
                name: "User".into(),
                fields: vec![
                    lock_field("id", "uuid", &["-pk"]),
                    lock_field("age", "int", &[]),
                ],
                indexes: vec![],
            }],
        };
        let op = MigrationOp::AddField {
            model: "User".into(),
            field: "age".into(),
            ty: "int".into(),
            nullable: false,
            uniq: false,
        };
        let stmts = op_to_sql(SqlDialect::Sqlite, &op, &target, Some(&current));
        // 6-step rebuild: PRAGMA off, CREATE shadow, INSERT, DROP, RENAME, PRAGMA on.
        assert_eq!(stmts.len(), 6);
        assert!(
            stmts
                .iter()
                .any(|s| s.contains("CREATE TABLE \"_osdl_new_users\""))
        );
        assert!(
            stmts
                .iter()
                .any(|s| s.contains("INSERT INTO \"_osdl_new_users\""))
        );
        assert!(stmts.iter().any(|s| s.contains("DROP TABLE \"users\"")));
        assert!(
            stmts
                .iter()
                .any(|s| s.contains("ALTER TABLE \"_osdl_new_users\" RENAME TO \"users\""))
        );
    }

    #[test]
    fn sqlite_drop_field_rebuilds_table() {
        let current = Lockfile {
            version: 1,
            checksum: String::new(),
            models: vec![LockModel {
                name: "User".into(),
                fields: vec![
                    lock_field("id", "uuid", &["-pk"]),
                    lock_field("age", "int", &[]),
                ],
                indexes: vec![],
            }],
        };
        let target = Lockfile {
            version: 1,
            checksum: String::new(),
            models: vec![LockModel {
                name: "User".into(),
                fields: vec![lock_field("id", "uuid", &["-pk"])],
                indexes: vec![],
            }],
        };
        let op = MigrationOp::DropField {
            model: "User".into(),
            field: "age".into(),
        };
        let stmts = op_to_sql(SqlDialect::Sqlite, &op, &target, Some(&current));
        assert!(stmts.len() >= 5);
        assert!(stmts.iter().any(|s| s.contains("\"_osdl_new_users\"")));
    }
}
