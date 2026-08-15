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
}

impl SqlDialect {
    /// Detect the dialect from a SeaORM connection URL scheme.
    pub fn from_url(url: &str) -> Option<Self> {
        if url.starts_with("sqlite:") {
            Some(SqlDialect::Sqlite)
        } else if url.starts_with("postgres:") || url.starts_with("postgresql:") {
            Some(SqlDialect::Postgres)
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
    let name = quote_ident(&field.name);
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
            quote_ident(&table_name(ref_model)),
            quote_ident(ref_field)
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
    let _ = model;
    def
}

/// Build the `CREATE TABLE` statement for a model from its lockfile projection.
pub fn create_table_sql(dialect: SqlDialect, model: &LockModel) -> String {
    use crate::naming::quote_ident;
    use crate::naming::table_name;
    let cols: Vec<String> = model
        .fields
        .iter()
        .map(|f| column_def(dialect, model, f))
        .collect();
    format!(
        "CREATE TABLE {} (\n  {}\n)",
        quote_ident(&table_name(&model.name)),
        cols.join(",\n  ")
    )
}

/// Translate one [`MigrationOp`] into the DDL statement(s) that apply it.
///
/// `target` supplies the full desired model projection (needed to build a
/// complete `CREATE TABLE` with every column).
pub fn op_to_sql(
    dialect: SqlDialect,
    op: &MigrationOp,
    target: &osdl_core::lockfile::Lockfile,
) -> Vec<String> {
    use crate::naming::{quote_ident, table_name};
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
            vec![format!("DROP TABLE {}", quote_ident(&table_name(model)))]
        }
        MigrationOp::AddField {
            model,
            field,
            ty,
            nullable,
            uniq,
        } => {
            let col_ty = if as_reference(ty).is_some() {
                column_type(dialect, "uuid")
            } else {
                column_type(dialect, ty)
            };
            let mut def = format!(
                "ALTER TABLE {} ADD COLUMN {} {}",
                quote_ident(&table_name(model)),
                quote_ident(field),
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
            vec![format!(
                "ALTER TABLE {} DROP COLUMN {}",
                quote_ident(&table_name(model)),
                quote_ident(field)
            )]
        }
        MigrationOp::AlterField {
            model,
            field,
            new_ty,
            nullable,
            uniq,
        } => {
            // SQLite cannot alter a column's type in place; Postgres can.
            match dialect {
                SqlDialect::Postgres => {
                    let col_ty = if as_reference(new_ty).is_some() {
                        column_type(dialect, "uuid")
                    } else {
                        column_type(dialect, new_ty)
                    };
                    let def = format!(
                        "ALTER TABLE {} ALTER COLUMN {} TYPE {}",
                        quote_ident(&table_name(model)),
                        quote_ident(field),
                        col_ty
                    );
                    let _ = nullable;
                    let _ = uniq;
                    vec![def]
                }
                SqlDialect::Sqlite => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use osdl_core::ast::LockModel;
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
        let stmts = op_to_sql(SqlDialect::Sqlite, &op, &lf);
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0], "ALTER TABLE \"users\" ADD COLUMN \"age\" INTEGER");
    }
}
