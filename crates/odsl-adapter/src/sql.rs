//! SQL DDL generation for the SeaORM-backed adapters (SQLite & Postgres).
//!
//! ODSL scalar keywords map to backend-appropriate column types, and
//! `Model.field` references become foreign-key columns. All identifiers are
//! double-quoted (ANSI SQL) so reserved words and mixed case are safe.

use odsl_core::ast::LockModel;
use odsl_core::ast::LockSeed;
use odsl_core::lockfile::Lockfile;
use odsl_migrator::MigrationOp;

use crate::AdapterError;

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

    /// Map to SeaORM's `DbBackend` for raw-statement construction.
    pub fn to_db_backend(self) -> sea_orm::DbBackend {
        match self {
            SqlDialect::Sqlite => sea_orm::DbBackend::Sqlite,
            SqlDialect::Postgres => sea_orm::DbBackend::Postgres,
            SqlDialect::Mysql => sea_orm::DbBackend::MySql,
        }
    }
}

/// Map an ODSL scalar keyword to a column type for the dialect.
/// Map an ODSL type keyword to the SQL column type for `dialect`.
///
/// For `numeric`, an explicit `(precision, scale)` is honoured when provided;
/// otherwise a documented default is used (SQLite/Postgres `NUMERIC`, MySQL
/// `DECIMAL(38,10)`). The default was previously hard-coded per-dialect and
/// arbitrary — exposing it via `-precision`/`-scale` makes it explicit and
/// round-trippable.
pub fn column_type(
    dialect: SqlDialect,
    keyword: &str,
    precision: Option<u16>,
    scale: Option<u16>,
) -> String {
    // Resolve numeric precision/scale with documented per-dialect defaults.
    let numeric = match (dialect, precision, scale) {
        (_, Some(p), Some(s)) => format!("({p},{s})"),
        (_, Some(p), None) => format!("({p})"),
        (SqlDialect::Mysql, None, None) => "(38,10)".to_string(),
        _ => String::new(),
    };
    let base = match (dialect, keyword) {
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
        (SqlDialect::Sqlite, "numeric") => &format!("NUMERIC{numeric}"),
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
        (SqlDialect::Postgres, "numeric") => &format!("NUMERIC{numeric}"),
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
        (SqlDialect::Mysql, "numeric") => &format!("DECIMAL{numeric}"),
        // Unknown keyword -> safest portable type.
        (_, _) => "TEXT",
    };
    base.to_string()
}

/// An ODSL field's stored `ty` may be a reference (`Model.field`).
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
fn column_def(
    dialect: SqlDialect,
    field: &odsl_core::ast::LockField,
    lf: &Lockfile,
    pk_cols: &[String],
) -> String {
    use crate::naming::*;
    let name = quote_ident_for(dialect, &field.name);
    let is_pk = field.intents.iter().any(|i| i == "-pk");
    let is_uniq = field.intents.iter().any(|i| i == "-uniq");
    let is_null = field.intents.iter().any(|i| i == "-null");
    let is_auto = field.intents.iter().any(|i| i == "-auto");

    let base_ty = if let Some((ref_model, ref_field)) = as_reference(&field.ty) {
        // Foreign key: use the *referenced model's* PK type (uuid/int/...),
        // NOT a hard-coded uuid — a mismatch here breaks the join and the FK
        // constraint on Postgres/MySQL.
        let pk_kw = referenced_pk_keyword(lf, ref_model);
        format!(
            "{} REFERENCES {}({})",
            column_type(dialect, pk_kw, None, None),
            quote_ident_for(dialect, &table_name(ref_model)),
            quote_ident_for(dialect, ref_field)
        )
    } else {
        column_type(
            dialect,
            &field.ty,
            field.numeric_precision,
            field.numeric_scale,
        )
        .to_string()
    };

    let mut def = format!("{name} {base_ty}");
    // Inline PRIMARY KEY only for a single-column key. Composite keys are
    // emitted as a table-level PRIMARY KEY (...) constraint by create_table_sql.
    let inline_pk = pk_cols.len() == 1 && pk_cols[0] == field.name;
    if inline_pk {
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
    def
}

/// Resolve the referenced model's primary-key scalar keyword (e.g. `uuid`,
/// `int`), falling back to `uuid` only when the model cannot be found (it
/// always should be — the validator guarantees references resolve).
fn referenced_pk_keyword<'a>(lf: &'a Lockfile, ref_model: &str) -> &'a str {
    let Some(m) = lf.model_by_name(ref_model) else {
        return "uuid";
    };
    let Some(pk) = m
        .fields
        .iter()
        .find(|f| f.intents.iter().any(|i| i == "-pk"))
    else {
        return "uuid";
    };
    &pk.ty
}

/// Build the `CREATE TABLE` statement for a model from its lockfile projection.
pub fn create_table_sql(dialect: SqlDialect, model: &LockModel, lf: &Lockfile) -> String {
    use crate::naming::{quote_ident_for, table_name};
    let pk_cols = model.pk_columns().to_vec();
    let cols: Vec<String> = model
        .fields
        .iter()
        .map(|f| column_def(dialect, f, lf, &pk_cols))
        .collect();
    // Composite primary key: emit a table-level PRIMARY KEY (...) constraint
    // instead of inline `PRIMARY KEY` on each column.
    let extra = if pk_cols.len() > 1 {
        let pk = pk_cols
            .iter()
            .map(|c| quote_ident_for(dialect, c))
            .collect::<Vec<_>>()
            .join(", ");
        format!(",\n  PRIMARY KEY ({pk})")
    } else {
        String::new()
    };
    format!(
        "CREATE TABLE {} (\n  {}{}\n)",
        quote_ident_for(dialect, &table_name(&model.name)),
        cols.join(",\n  "),
        extra
    )
}

/// Translate one [`MigrationOp`] into the DDL statement(s) that apply it.
///
/// `target` supplies the full desired (post-migration) model projection.
/// `current` (when available) supplies the prior projection, which is required
/// to rebuild a SQLite table in place for operations SQLite cannot do with a
/// single `ALTER` (adding a NOT NULL / foreign-key column, dropping a column,
/// or changing a column's type).
/// Render a `CREATE VIEW` / `CREATE MATERIALIZED VIEW` statement for a `LockView`.
pub fn create_view_sql(dialect: SqlDialect, view: &odsl_core::ast::LockView) -> String {
    use crate::naming::quote_ident_for;
    let kw = if view.materialized && dialect == SqlDialect::Postgres {
        "CREATE MATERIALIZED VIEW"
    } else {
        "CREATE VIEW"
    };
    format!(
        "{} {} AS {}",
        kw,
        quote_ident_for(dialect, &view.name),
        view.query.trim()
    )
}

/// Render `INSERT` statements for a `LockSeed` dataset. Each row becomes one
/// `INSERT INTO <table> (cols) VALUES (vals)`; values are emitted verbatim
/// (already validated by the parser/validator). Values containing single
/// quotes are escaped by doubling. Returns one statement per row.
pub fn create_seed_sql(dialect: SqlDialect, seed: &LockSeed, table: &str) -> Vec<String> {
    use crate::naming::quote_ident_for;
    let table_ident = quote_ident_for(dialect, table);
    let mut stmts = Vec::new();
    for row in &seed.rows {
        if row.is_empty() {
            continue;
        }
        let cols: Vec<String> = row
            .iter()
            .map(|(c, _)| quote_ident_for(dialect, c))
            .collect();
        let vals: Vec<String> = row
            .iter()
            .map(|(_, v)| format!("'{}'", v.replace('\'', "''")))
            .collect();
        stmts.push(format!(
            "INSERT INTO {} ({}) VALUES ({});",
            table_ident,
            cols.join(", "),
            vals.join(", ")
        ));
    }
    stmts
}

pub fn op_to_sql(
    dialect: SqlDialect,
    op: &MigrationOp,
    target: &odsl_core::lockfile::Lockfile,
    current: Option<&odsl_core::lockfile::Lockfile>,
) -> Result<Vec<String>, AdapterError> {
    use crate::naming::{quote_ident_for, table_name};
    match op {
        MigrationOp::CreateModel { model } => {
            let m = target.model_by_name(model).ok_or_else(|| {
                AdapterError::Render(format!(
                    "create-model op references unknown model `{model}` (not present in target lockfile)"
                ))
            })?;
            Ok(vec![create_table_sql(dialect, m, target)])
        }
        MigrationOp::DropModel { model } => Ok(vec![format!(
            "DROP TABLE {}",
            quote_ident_for(dialect, &table_name(model))
        )]),
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
                && (!*nullable || is_fk || *uniq)
                && let Some(cur) = current
                && let Some(old) = cur.model_by_name(model)
                && let Some(new_lm) = target.model_by_name(model)
            {
                let mut fields = new_lm.fields.clone();
                // Ensure the new field is present even if `target` somehow lags.
                if !fields.iter().any(|f| f.name == *field) {
                    // A freshly-added FK column takes the referenced PK type.
                    let col_ty = if is_fk {
                        referenced_pk_keyword(target, &split_ref(ty).0).to_string()
                    } else {
                        ty.clone()
                    };
                    let mut intents = vec![];
                    if !*nullable {
                        intents.push("-null".to_string());
                    }
                    if *uniq {
                        intents.push("-uniq".to_string());
                    }
                    fields.push(odsl_core::ast::LockField {
                        name: field.clone(),
                        ty: col_ty,
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
                    });
                }
                return sqlite_rebuild_sql(old, model, &fields, target);
            }
            // Non-rebuild path (Postgres/MySQL, or a nullable non-FK,
            // non-unique column on SQLite).
            let col_ty = if is_fk {
                column_type(
                    dialect,
                    referenced_pk_keyword(target, &split_ref(ty).0),
                    None,
                    None,
                )
            } else {
                column_type(dialect, ty, None, None)
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
            Ok(vec![def])
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
                return sqlite_rebuild_sql(old, model, &kept, target);
            }
            Ok(vec![format!(
                "ALTER TABLE {} DROP COLUMN {}",
                quote_ident_for(dialect, &table_name(model)),
                quote_ident_for(dialect, field)
            )])
        }
        MigrationOp::AlterField {
            model,
            field,
            new_ty,
            nullable,
            uniq,
        } => {
            // SQLite cannot alter a column in place; Postgres/MySQL can (and
            // need separate statements for type, nullability and uniqueness).
            match dialect {
                SqlDialect::Postgres | SqlDialect::Mysql => {
                    let is_fk = as_reference(new_ty).is_some();
                    let col_ty = if is_fk {
                        column_type(
                            dialect,
                            referenced_pk_keyword(target, &split_ref(new_ty).0),
                            None,
                            None,
                        )
                    } else {
                        column_type(dialect, new_ty, None, None)
                    };
                    let mut stmts = vec![format!(
                        "ALTER TABLE {} ALTER COLUMN {} TYPE {}",
                        quote_ident_for(dialect, &table_name(model)),
                        quote_ident_for(dialect, field),
                        col_ty
                    )];
                    // Nullability change: separate SET/DROP NOT NULL statement.
                    if *nullable {
                        stmts.push(format!(
                            "ALTER TABLE {} ALTER COLUMN {} DROP NOT NULL",
                            quote_ident_for(dialect, &table_name(model)),
                            quote_ident_for(dialect, field)
                        ));
                    } else {
                        stmts.push(format!(
                            "ALTER TABLE {} ALTER COLUMN {} SET NOT NULL",
                            quote_ident_for(dialect, &table_name(model)),
                            quote_ident_for(dialect, field)
                        ));
                    }
                    // Uniqueness change: add/drop a unique constraint.
                    if *uniq {
                        stmts.push(format!(
                            "ALTER TABLE {} ADD CONSTRAINT {}_{}_key UNIQUE ({})",
                            quote_ident_for(dialect, &table_name(model)),
                            table_name(model),
                            field,
                            quote_ident_for(dialect, field)
                        ));
                    } else {
                        stmts.push(format!(
                            "ALTER TABLE {} DROP CONSTRAINT IF EXISTS {}_{}_key",
                            quote_ident_for(dialect, &table_name(model)),
                            table_name(model),
                            field
                        ));
                    }
                    Ok(stmts)
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
                            f.ty = if is_fk {
                                referenced_pk_keyword(target, &split_ref(new_ty).0).to_string()
                            } else {
                                new_ty.clone()
                            };
                            f.intents.retain(|i| i != "-null");
                            if !*nullable && !f.intents.iter().any(|i| i == "-null") {
                                f.intents.push("-null".into());
                            }
                            f.intents.retain(|i| i != "-uniq");
                            if *uniq && !f.intents.iter().any(|i| i == "-uniq") {
                                f.intents.push("-uniq".into());
                            }
                        }
                        return sqlite_rebuild_sql(old, model, &fields, target);
                    }
                    // Best-effort: SQLite ignores the type but renaming to the
                    // same name is a no-op for type; emit a documented guard.
                    Ok(vec![format!(
                        "-- sqlite: ALTER COLUMN unsupported; manual migration needed for {}.{}",
                        table_name(model),
                        field
                    )])
                }
            }
        }
        MigrationOp::CreateView { view } => {
            let v = target
                .view_by_name(view)
                .ok_or_else(|| {
                    AdapterError::Render(format!(
                        "create-view op references unknown view `{view}` (not present in target lockfile)"
                    ))
                })?;
            Ok(vec![create_view_sql(dialect, v)])
        }
        MigrationOp::DropView { view } => Ok(vec![format!(
            "DROP VIEW IF EXISTS {}",
            quote_ident_for(dialect, view)
        )]),
        MigrationOp::SeedData { model } => {
            // Find the seed dataset for this model in the target lockfile and
            // render one INSERT per row. If the model has no seed entry (should
            // not happen for a well-formed plan), emit nothing.
            let table = table_name(model);
            if let Some(seed) = target.seeds.iter().find(|s| s.model == *model) {
                Ok(create_seed_sql(dialect, seed, &table))
            } else {
                Ok(vec![])
            }
        }
    }
}

/// Split a reference type keyword (`Model.field`) into its parts.
fn split_ref(ty: &str) -> (String, String) {
    match ty.split_once('.') {
        Some((m, c)) => (m.to_string(), c.to_string()),
        None => (ty.to_string(), "id".to_string()),
    }
}

/// SQLite has no in-place `ADD COLUMN ... NOT NULL` (on a non-empty table),
/// no `ADD COLUMN ... REFERENCES`, no `DROP COLUMN` on old versions, and no
/// `ALTER COLUMN`. The portable workaround is a full table rebuild:
///
/// 1. disable foreign keys
/// 2. create a shadow table `_odsl_new_<tbl>` with the desired columns
/// 3. copy rows across (only the columns shared by both schemas)
/// 4. drop the original table
/// 5. rename the shadow table into place
/// 6. re-enable foreign keys
///
/// A freshly-added foreign-key column is made nullable in the shadow table and
/// filled with `NULL` during the copy, because a NOT NULL uuid/text column
/// cannot be populated from existing rows that have no such value.
fn sqlite_rebuild_sql(
    old: &LockModel,
    new_model: &str,
    new_fields: &[odsl_core::ast::LockField],
    lf: &Lockfile,
) -> Result<Vec<String>, AdapterError> {
    use crate::naming::{quote_ident, table_name};
    let tbl = table_name(new_model);
    let shadow = format!("_odsl_new_{}", tbl);
    // FK columns that are newly added must be nullable in the shadow so the
    // copy INSERT can populate them with NULL.
    let mut shadow_fields: Vec<odsl_core::ast::LockField> = new_fields
        .iter()
        .map(|f| {
            let is_new_fk =
                as_reference(&f.ty).is_some() && !old.fields.iter().any(|of| of.name == f.name);
            if is_new_fk {
                let mut f = f.clone();
                if !f.intents.iter().any(|i| i == "-null") {
                    f.intents.push("-null".to_string());
                }
                f
            } else {
                f.clone()
            }
        })
        .collect();
    shadow_fields.sort_by(|a, b| a.name.cmp(&b.name));
    let new_lm = LockModel {
        name: new_model.to_string(),
        fields: shadow_fields,
        indexes: vec![],
        primary_key: vec![],
    };
    let new_cols: Vec<String> = new_lm.fields.iter().map(|f| quote_ident(&f.name)).collect();
    // For the INSERT, select old columns where they exist; for new columns
    // (present in the target but not the old schema) emit a type-appropriate
    // default literal (NULL for references) so the constraint is satisfied.
    let select_exprs: Vec<String> = new_lm
        .fields
        .iter()
        .map(|f| {
            if old.fields.iter().any(|of| of.name == f.name) {
                quote_ident(&f.name)
            } else {
                default_literal(f)
            }
        })
        .collect();

    Ok(vec![
        "PRAGMA foreign_keys=off;".to_string(),
        create_table_sql(SqlDialect::Sqlite, &new_lm, lf).replace(
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
    ])
}

/// A type-appropriate DEFAULT literal for a freshly-added column so the SQLite
/// rebuild INSERT can populate it. References use `NULL` (the shadow column is
/// made nullable); scalars use `0` / `0.0` / `''`.
fn default_literal(field: &odsl_core::ast::LockField) -> String {
    if as_reference(&field.ty).is_some() {
        "NULL".to_string()
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
    use odsl_core::ast::LockModel;
    use odsl_core::lockfile::Lockfile;
    use odsl_core::lockfile::lock_field;

    #[test]
    fn column_type_numeric_precision_is_explicit() {
        // Without precision, MySQL falls back to the documented DECIMAL(38,10)
        // and PG/SQLite use bare NUMERIC.
        assert_eq!(
            column_type(SqlDialect::Mysql, "numeric", None, None),
            "DECIMAL(38,10)"
        );
        assert_eq!(
            column_type(SqlDialect::Postgres, "numeric", None, None),
            "NUMERIC"
        );
        assert_eq!(
            column_type(SqlDialect::Sqlite, "numeric", None, None),
            "NUMERIC"
        );
        // Explicit precision/scale is honoured across dialects.
        assert_eq!(
            column_type(SqlDialect::Mysql, "numeric", Some(18), Some(4)),
            "DECIMAL(18,4)"
        );
        assert_eq!(
            column_type(SqlDialect::Postgres, "numeric", Some(18), Some(4)),
            "NUMERIC(18,4)"
        );
        // Precision only (no scale) is allowed.
        assert_eq!(
            column_type(SqlDialect::Mysql, "numeric", Some(10), None),
            "DECIMAL(10)"
        );
        // Non-numeric types ignore precision.
        assert_eq!(
            column_type(SqlDialect::Postgres, "int", Some(18), Some(4)),
            "INTEGER"
        );
    }

    /// An empty lockfile (used for FK-resolution fallbacks in non-FK tests).
    fn empty_lf() -> Lockfile {
        Lockfile {
            seeds: vec![],
            version: 1,
            checksum: String::new(),
            models: vec![],
            views: vec![],
        }
    }

    fn user_model() -> LockModel {
        LockModel {
            name: "User".into(),
            fields: vec![
                lock_field("id", "uuid", &["-pk"]),
                lock_field("email", "string", &["-uniq"]),
            ],
            indexes: vec![],
            primary_key: vec![],
        }
    }

    fn composite_pk_model() -> LockModel {
        LockModel {
            name: "Membership".into(),
            fields: vec![
                lock_field("tenant_id", "uuid", &[]),
                lock_field("user_id", "uuid", &[]),
                lock_field("role", "string", &[]),
            ],
            indexes: vec![],
            // Composite key declared at the model level.
            primary_key: vec!["tenant_id".into(), "user_id".into()],
        }
    }

    #[test]
    fn creates_composite_primary_key_sqlite() {
        let sql = create_table_sql(SqlDialect::Sqlite, &composite_pk_model(), &empty_lf());
        assert!(sql.contains("CREATE TABLE \"memberships\""));
        // No inline PRIMARY KEY on individual columns.
        assert!(!sql.contains("\"tenant_id\" TEXT PRIMARY KEY"));
        assert!(!sql.contains("\"user_id\" TEXT PRIMARY KEY"));
        // A table-level composite PRIMARY KEY constraint.
        assert!(sql.contains("PRIMARY KEY (\"tenant_id\", \"user_id\")"));
    }

    #[test]
    fn creates_composite_primary_key_postgres() {
        let sql = create_table_sql(SqlDialect::Postgres, &composite_pk_model(), &empty_lf());
        assert!(sql.contains("PRIMARY KEY (\"tenant_id\", \"user_id\")"));
    }

    #[test]
    fn creates_composite_primary_key_mysql() {
        let sql = create_table_sql(SqlDialect::Mysql, &composite_pk_model(), &empty_lf());
        // MySQL backtick-quotes the key columns.
        assert!(sql.contains("PRIMARY KEY (`tenant_id`, `user_id`)"));
    }

    #[test]
    fn creates_sqlite_table() {
        let sql = create_table_sql(SqlDialect::Sqlite, &user_model(), &empty_lf());
        assert!(sql.contains("CREATE TABLE \"users\""));
        assert!(sql.contains("\"id\" TEXT PRIMARY KEY"));
        assert!(sql.contains("\"email\" TEXT"));
        assert!(sql.contains("UNIQUE"));
        assert!(sql.contains("NOT NULL"));
    }

    #[test]
    fn creates_mysql_table_with_backticks() {
        let sql = create_table_sql(SqlDialect::Mysql, &user_model(), &empty_lf());
        // MySQL uses backtick quoting and CHAR(36) for uuid.
        assert!(sql.contains("CREATE TABLE `users`"));
        assert!(sql.contains("`id` CHAR(36) PRIMARY KEY"));
        assert!(sql.contains("`email` TEXT"));
        assert!(sql.contains("UNIQUE"));
    }

    #[test]
    fn renders_seed_inserts() {
        let seed = odsl_core::ast::LockSeed {
            model: "User".into(),
            rows: vec![vec![
                ("id".into(), "00000000-0000-0000-0000-000000000001".into()),
                ("email".into(), "root@odsl.dev".into()),
            ]],
        };
        let stmts = create_seed_sql(SqlDialect::Postgres, &seed, "users");
        assert_eq!(stmts.len(), 1);
        let s = &stmts[0];
        assert!(s.starts_with("INSERT INTO \"users\" (\"id\", \"email\") VALUES ("));
        assert!(s.contains("'00000000-0000-0000-0000-000000000001'"));
        assert!(s.contains("'root@odsl.dev'"));
        assert!(s.ends_with(");"));
        // Single quotes in values are escaped by doubling.
        let seed2 = odsl_core::ast::LockSeed {
            model: "User".into(),
            rows: vec![vec![("bio".into(), "it's me".into())]],
        };
        let s2 = &create_seed_sql(SqlDialect::Sqlite, &seed2, "users")[0];
        assert!(s2.contains("'it''s me'"));
    }

    #[test]
    fn creates_postgres_table() {
        let sql = create_table_sql(SqlDialect::Postgres, &user_model(), &empty_lf());
        assert!(sql.contains("CREATE TABLE \"users\""));
        assert!(sql.contains("\"id\" UUID PRIMARY KEY"));
        assert!(sql.contains("\"email\" TEXT"));
        assert!(sql.contains("UNIQUE"));
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
            primary_key: vec![],
        };
        let sql = create_table_sql(SqlDialect::Mysql, &m, &empty_lf());
        assert!(sql.contains("CHAR(36)"));
        assert!(sql.contains("TEXT"));
        assert!(sql.contains("BIGINT"));
        assert!(sql.contains("DOUBLE"));
        assert!(sql.contains("TINYINT(1)"));
        assert!(sql.contains("DATETIME"));
    }

    #[test]
    fn mysql_add_field_is_inline() {
        let lf = odsl_core::lockfile::Lockfile {
            seeds: vec![],
            version: 1,
            checksum: String::new(),
            models: vec![user_model()],
            views: vec![],
        };
        let op = MigrationOp::AddField {
            model: "User".into(),
            field: "age".into(),
            ty: "int".into(),
            nullable: true,
            uniq: false,
        };
        let stmts = op_to_sql(SqlDialect::Mysql, &op, &lf, None).unwrap();
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0], "ALTER TABLE `users` ADD COLUMN `age` INT");
    }

    #[test]
    fn mysql_alter_field_changes_type() {
        let lf = odsl_core::lockfile::Lockfile {
            seeds: vec![],
            version: 1,
            checksum: String::new(),
            models: vec![user_model()],
            views: vec![],
        };
        let op = MigrationOp::AlterField {
            model: "User".into(),
            field: "age".into(),
            new_ty: "bigint".into(),
            nullable: true,
            uniq: false,
        };
        let stmts = op_to_sql(SqlDialect::Mysql, &op, &lf, None).unwrap();
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
            primary_key: vec![],
        };
        let lf_with_user = Lockfile {
            seeds: vec![],
            version: 1,
            checksum: String::new(),
            models: vec![user_model()],
            views: vec![],
        };
        let sql = create_table_sql(SqlDialect::Postgres, &m, &lf_with_user);
        assert!(sql.contains("REFERENCES \"users\"(\"id\")"));
    }

    #[test]
    fn fk_resolves_referenced_pk_type() {
        // A FK must take the *referenced model's* PK type, not a hard-coded uuid.
        // Here User.id is `int`, so the FK column must be INTEGER (not TEXT/UUID).
        let user = LockModel {
            name: "User".into(),
            fields: vec![lock_field("id", "int", &["-pk"])],
            indexes: vec![],
            primary_key: vec![],
        };
        let post = LockModel {
            name: "Post".into(),
            fields: vec![
                lock_field("id", "uuid", &["-pk"]),
                lock_field("author", "User.id", &[]),
            ],
            indexes: vec![],
            primary_key: vec![],
        };
        let lf = Lockfile {
            seeds: vec![],
            version: 1,
            checksum: String::new(),
            models: vec![user, post],
            views: vec![],
        };
        let sql = create_table_sql(SqlDialect::Postgres, lf.model_by_name("Post").unwrap(), &lf);
        // INTEGER FK (matching the int PK), not UUID.
        assert!(
            sql.contains("\"author\" INTEGER REFERENCES \"users\"(\"id\")"),
            "expected int-typed FK, got:\n{sql}"
        );
    }

    #[test]
    fn add_field_sql() {
        let lf = odsl_core::lockfile::Lockfile {
            seeds: vec![],
            version: 1,
            checksum: String::new(),
            models: vec![user_model()],
            views: vec![],
        };
        let op = MigrationOp::AddField {
            model: "User".into(),
            field: "age".into(),
            ty: "int".into(),
            nullable: true,
            uniq: false,
        };
        let stmts = op_to_sql(SqlDialect::Sqlite, &op, &lf, None).unwrap();
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0], "ALTER TABLE \"users\" ADD COLUMN \"age\" INTEGER");
    }

    #[test]
    fn sqlite_add_not_null_rebuilds_table() {
        // Adding a NOT NULL column to a NON-empty table requires a full rebuild
        // on SQLite (no in-place ADD COLUMN ... NOT NULL). With `current`
        // present, op_to_sql must emit the shadow-table rebuild sequence.
        let current = Lockfile {
            seeds: vec![],
            version: 1,
            checksum: String::new(),
            models: vec![LockModel {
                name: "User".into(),
                fields: vec![lock_field("id", "uuid", &["-pk"])],
                indexes: vec![],
                primary_key: vec![],
            }],
            views: vec![],
        };
        let target = Lockfile {
            seeds: vec![],
            version: 1,
            checksum: String::new(),
            models: vec![LockModel {
                name: "User".into(),
                fields: vec![
                    lock_field("id", "uuid", &["-pk"]),
                    lock_field("age", "int", &[]),
                ],
                indexes: vec![],
                primary_key: vec![],
            }],
            views: vec![],
        };
        let op = MigrationOp::AddField {
            model: "User".into(),
            field: "age".into(),
            ty: "int".into(),
            nullable: false,
            uniq: false,
        };
        let stmts = op_to_sql(SqlDialect::Sqlite, &op, &target, Some(&current)).unwrap();
        // 6-step rebuild: PRAGMA off, CREATE shadow, INSERT, DROP, RENAME, PRAGMA on.
        assert_eq!(stmts.len(), 6);
        assert!(
            stmts
                .iter()
                .any(|s| s.contains("CREATE TABLE \"_odsl_new_users\""))
        );
        assert!(
            stmts
                .iter()
                .any(|s| s.contains("INSERT INTO \"_odsl_new_users\""))
        );
        assert!(stmts.iter().any(|s| s.contains("DROP TABLE \"users\"")));
        assert!(
            stmts
                .iter()
                .any(|s| s.contains("ALTER TABLE \"_odsl_new_users\" RENAME TO \"users\""))
        );
    }

    #[test]
    fn sqlite_drop_field_rebuilds_table() {
        let current = Lockfile {
            seeds: vec![],
            version: 1,
            checksum: String::new(),
            models: vec![LockModel {
                name: "User".into(),
                fields: vec![
                    lock_field("id", "uuid", &["-pk"]),
                    lock_field("age", "int", &[]),
                ],
                indexes: vec![],
                primary_key: vec![],
            }],
            views: vec![],
        };
        let target = Lockfile {
            seeds: vec![],
            version: 1,
            checksum: String::new(),
            models: vec![LockModel {
                name: "User".into(),
                fields: vec![lock_field("id", "uuid", &["-pk"])],
                indexes: vec![],
                primary_key: vec![],
            }],
            views: vec![],
        };
        let op = MigrationOp::DropField {
            model: "User".into(),
            field: "age".into(),
        };
        let stmts = op_to_sql(SqlDialect::Sqlite, &op, &target, Some(&current)).unwrap();
        assert!(stmts.len() >= 5);
        assert!(stmts.iter().any(|s| s.contains("\"_odsl_new_users\"")));
    }
}
