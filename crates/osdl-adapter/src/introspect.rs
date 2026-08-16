//! Reverse-engineering: introspect a live SQL database and emit OSDL source.
//!
//! `osdl pull --db-url <url>` calls [`introspect_to_osdl`] to connect to the
//! database described by `url`, read its catalog (tables, columns, primary
//! keys, unique constraints, secondary indexes) and pretty-print an equivalent
//! OSDL schema. The result is a starting point for a human to refine — it is
//! intentionally lossy about things OSDL cannot express (composite non-unique
//! indexes are kept; check constraints, stored generators and triggers are
//! not).
//!
//! The connection is established with SeaORM (already a workspace dependency)
//! and catalog rows are read with raw `SELECT`/`PRAGMA` statements via
//! `query_all_raw`, so there is no compile-time SQL checking to fight and the
//! per-dialect differences live entirely in the catalog queries.

use std::fmt::Write as _;

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};

/// Dialect the introspector targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    Postgres,
    Mysql,
}

impl Dialect {
    /// Pick a dialect from a connection URL scheme.
    pub fn from_url(url: &str) -> Option<Self> {
        if url.starts_with("sqlite:") || url.starts_with("file:") {
            Some(Dialect::Sqlite)
        } else if url.starts_with("postgres:") {
            Some(Dialect::Postgres)
        } else if url.starts_with("mysql:") {
            Some(Dialect::Mysql)
        } else {
            None
        }
    }

    fn backend(self) -> DbBackend {
        match self {
            Dialect::Sqlite => DbBackend::Sqlite,
            Dialect::Postgres => DbBackend::Postgres,
            Dialect::Mysql => DbBackend::MySql,
        }
    }
}

/// A column read from the catalog.
struct ColumnInfo {
    table: String,
    name: String,
    data_type: String,
    not_null: bool,
    is_pk: bool,
    is_unique: bool,
    default: Option<String>,
}

/// A (non-PK) composite or single-column index read from the catalog.
struct IndexInfo {
    table: String,
    columns: Vec<String>,
    unique: bool,
}

/// Connect to `db_url` and return a connection plus the detected dialect.
async fn connect(db_url: &str) -> Result<(DatabaseConnection, Dialect), String> {
    let dialect = Dialect::from_url(db_url)
        .ok_or_else(|| format!("unsupported URL scheme for introspection: {db_url}"))?;
    let conn = Database::connect(db_url)
        .await
        .map_err(|e| format!("connecting to {db_url}: {e}"))?;
    Ok((conn, dialect))
}

/// Introspect `db_url` and return OSDL source text describing the schema.
pub async fn introspect_to_osdl(db_url: &str) -> Result<String, String> {
    let (conn, dialect) = connect(db_url).await?;
    let tables = list_tables(&conn, dialect).await?;

    let mut columns: Vec<ColumnInfo> = Vec::new();
    let mut indexes: Vec<IndexInfo> = Vec::new();
    for table in &tables {
        let (cols, idxs) = read_table(&conn, dialect, table).await?;
        columns.extend(cols);
        indexes.extend(idxs);
    }

    Ok(render_osdl(&tables, &columns, &indexes))
}

/// Run a raw catalog query and return its rows (positional access).
async fn fetch(
    conn: &DatabaseConnection,
    dialect: Dialect,
    sql: &str,
) -> Result<Vec<sea_orm::QueryResult>, String> {
    let stmt = Statement::from_string(dialect.backend(), sql.to_string());
    conn.query_all_raw(stmt)
        .await
        .map_err(|e| format!("catalog query failed: {e}"))
}

/// List user tables for the dialect (skips the OSDL history tracker and system
/// catalogs).
async fn list_tables(conn: &DatabaseConnection, dialect: Dialect) -> Result<Vec<String>, String> {
    let sql = match dialect {
        Dialect::Sqlite => {
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
        }
        Dialect::Postgres => {
            "SELECT table_name FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_type = 'BASE TABLE' \
             AND table_name <> '_osdl_migrations' ORDER BY table_name"
        }
        Dialect::Mysql => {
            "SELECT table_name FROM information_schema.tables \
             WHERE table_schema = DATABASE() AND table_type = 'BASE TABLE' \
             AND table_name <> '_osdl_migrations' ORDER BY table_name"
        }
    };
    let rows = fetch(conn, dialect, sql).await?;
    let mut out = Vec::new();
    for r in rows {
        if let Ok(name) = r.try_get_by_index::<String>(0) {
            out.push(name);
        }
    }
    Ok(out)
}

/// Read a single table's columns and indexes.
async fn read_table(
    conn: &DatabaseConnection,
    dialect: Dialect,
    table: &str,
) -> Result<(Vec<ColumnInfo>, Vec<IndexInfo>), String> {
    let columns = match dialect {
        Dialect::Sqlite => read_sqlite_columns(conn, table).await?,
        Dialect::Postgres => read_pg_columns(conn, table).await?,
        Dialect::Mysql => read_mysql_columns(conn, table).await?,
    };

    let indexes = read_indexes(conn, dialect, table).await?;
    Ok((columns, indexes))
}

/// SQLite columns via `PRAGMA table_info`.
///
/// Table names come from the catalog (trusted) and are additionally
/// identifier-sanitized; PRAGMA cannot bind parameters, so the name is
/// interpolated after validation.
async fn read_sqlite_columns(
    conn: &DatabaseConnection,
    table: &str,
) -> Result<Vec<ColumnInfo>, String> {
    let table = sanitize_ident(table)?;
    let rows = fetch(
        conn,
        Dialect::Sqlite,
        &format!("PRAGMA table_info('{table}')"),
    )
    .await?;
    // pk is column index 5, notnull is 3, dflt_value is 4.
    let mut cols = Vec::new();
    for r in rows {
        let name: String = r.try_get_by_index(1).map_err(|e| e.to_string())?;
        let data_type: String = r.try_get_by_index(2).map_err(|e| e.to_string())?;
        let not_null: i32 = r.try_get_by_index(3).map_err(|e| e.to_string())?;
        let pk: i32 = r.try_get_by_index(5).map_err(|e| e.to_string())?;
        let default: Option<String> = r.try_get_by_index(4).ok().flatten();
        cols.push(ColumnInfo {
            table: table.to_string(),
            name,
            data_type,
            not_null: not_null != 0,
            is_pk: pk != 0,
            is_unique: false, // resolved from index_list below
            default,
        });
    }
    // Promote PK + unique flag from index list.
    let idx_rows = fetch(
        conn,
        Dialect::Sqlite,
        &format!("PRAGMA index_list('{table}')"),
    )
    .await?;
    for ir in idx_rows {
        // PRAGMA index_list columns: seq(0), name(1), unique(2), origin(3), partial(4).
        let iname: String = ir.try_get_by_index(1).map_err(|e| e.to_string())?;
        let unique: i32 = ir.try_get_by_index(2).map_err(|e| e.to_string())?;
        let origin: String = ir.try_get_by_index(3).unwrap_or_default();
        if origin == "pk" {
            continue; // handled by table_info pk flag
        }
        if unique == 0 {
            continue;
        }
        // A unique index with a single column => mark that column unique.
        let info_rows = fetch(
            conn,
            Dialect::Sqlite,
            &format!("PRAGMA index_info('{iname}')"),
        )
        .await?;
        for info in info_rows {
            let cname: String = info.try_get_by_index(2).map_err(|e| e.to_string())?;
            if let Some(c) = cols.iter_mut().find(|c| c.name == cname) {
                c.is_unique = true;
            }
        }
    }
    Ok(cols)
}

/// Postgres columns from `information_schema.columns` + key metadata.
async fn read_pg_columns(
    conn: &DatabaseConnection,
    table: &str,
) -> Result<Vec<ColumnInfo>, String> {
    let sql = format!(
        "SELECT c.column_name, c.data_type, c.is_nullable, c.column_default, \
                CASE WHEN pk.attname IS NOT NULL THEN 1 ELSE 0 END AS is_pk, \
                CASE WHEN u.attname IS NOT NULL THEN 1 ELSE 0 END AS is_unique \
         FROM information_schema.columns c \
         LEFT JOIN ( \
             SELECT a.attname FROM pg_index i \
             JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey) \
             WHERE i.indisprimary AND i.indrelid = '{table}'::regclass \
         ) pk ON pk.attname = c.column_name \
         LEFT JOIN ( \
             SELECT a.attname FROM pg_index i \
             JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey) \
             WHERE i.indisunique AND NOT i.indisprimary AND i.indrelid = '{table}'::regclass \
         ) u ON u.attname = c.column_name \
         WHERE c.table_schema = 'public' AND c.table_name = '{table}' \
         ORDER BY c.ordinal_position"
    );
    let rows = fetch(conn, Dialect::Postgres, &sql).await?;
    let mut out = Vec::new();
    for r in rows {
        let name: String = r.try_get_by_index(0).unwrap_or_default();
        let data_type: String = r.try_get_by_index(1).unwrap_or_default();
        let not_null: String = r.try_get_by_index(2).unwrap_or_default();
        let is_pk: i64 = r.try_get_by_index(4).unwrap_or(0);
        let is_unique: i64 = r.try_get_by_index(5).unwrap_or(0);
        let default: Option<String> = r.try_get_by_index(3).ok().flatten();
        out.push(ColumnInfo {
            table: table.to_string(),
            name,
            data_type,
            not_null: not_null == "NO",
            is_pk: is_pk != 0,
            is_unique: is_unique != 0,
            default,
        });
    }
    Ok(out)
}

/// MySQL columns from `information_schema.columns` + key metadata.
async fn read_mysql_columns(
    conn: &DatabaseConnection,
    table: &str,
) -> Result<Vec<ColumnInfo>, String> {
    let sql = format!(
        "SELECT c.COLUMN_NAME, c.DATA_TYPE, c.IS_NULLABLE, c.COLUMN_DEFAULT, \
                c.COLUMN_KEY \
         FROM information_schema.columns c \
         WHERE c.table_schema = DATABASE() AND c.table_name = '{table}' \
         ORDER BY c.ORDINAL_POSITION"
    );
    let rows = fetch(conn, Dialect::Mysql, &sql).await?;
    let mut out = Vec::new();
    for r in rows {
        let name: String = r.try_get_by_index(0).unwrap_or_default();
        let data_type: String = r.try_get_by_index(1).unwrap_or_default();
        let is_nullable: String = r.try_get_by_index(2).unwrap_or_default();
        let key: String = r.try_get_by_index(4).unwrap_or_default();
        let default: Option<String> = r.try_get_by_index(3).ok().flatten();
        out.push(ColumnInfo {
            table: table.to_string(),
            name,
            data_type,
            not_null: is_nullable == "NO",
            is_pk: key == "PRI",
            is_unique: key == "PRI" || key == "UNI",
            default,
        });
    }
    Ok(out)
}

/// Read secondary / unique indexes for a table (excluding pure PK indexes).
async fn read_indexes(
    conn: &DatabaseConnection,
    dialect: Dialect,
    table: &str,
) -> Result<Vec<IndexInfo>, String> {
    let rows = match dialect {
        Dialect::Sqlite => {
            let table = sanitize_ident(table)?;
            // Collect all index names then their columns.
            let idx_list = fetch(conn, dialect, &format!("PRAGMA index_list('{table}')")).await?;
            let mut out = Vec::new();
            for ir in idx_list {
                // PRAGMA index_list columns: seq(0), name(1), unique(2), origin(3), partial(4).
                let iname: String = ir.try_get_by_index(1).map_err(|e| e.to_string())?;
                let iname = sanitize_ident(&iname)?;
                let unique: i32 = ir.try_get_by_index(2).map_err(|e| e.to_string())?;
                let origin: String = ir.try_get_by_index(3).unwrap_or_default();
                if origin == "pk" {
                    continue;
                }
                let info = fetch(conn, dialect, &format!("PRAGMA index_info('{iname}')")).await?;
                let cols: Vec<String> = info
                    .into_iter()
                    .map(|r| r.try_get_by_index::<String>(2).unwrap_or_default())
                    .collect();
                if cols.is_empty() {
                    continue;
                }
                out.push(IndexInfo {
                    table: table.to_string(),
                    columns: cols,
                    unique: unique != 0,
                });
            }
            return Ok(out);
        }
        Dialect::Postgres => {
            let sql = format!(
                "SELECT i.relname AS index_name, a.attname AS column_name, \
                        ix.indisunique AS is_unique \
                 FROM pg_index ix \
                 JOIN pg_class i ON i.oid = ix.indexrelid \
                 JOIN pg_class t ON t.oid = ix.indrelid \
                 JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = ANY(ix.indkey) \
                 WHERE t.relname = '{table}' AND NOT ix.indisprimary \
                 ORDER BY i.relname, a.attnum"
            );
            fetch(conn, dialect, &sql).await?
        }
        Dialect::Mysql => {
            let sql = format!(
                "SELECT INDEX_NAME, COLUMN_NAME, NON_UNIQUE \
                 FROM information_schema.statistics \
                 WHERE table_schema = DATABASE() AND table_name = '{table}' \
                 AND INDEX_NAME <> 'PRIMARY' \
                 ORDER BY INDEX_NAME, SEQ_IN_INDEX"
            );
            fetch(conn, dialect, &sql).await?
        }
    };

    // Group rows into per-index entries.
    let mut by_name: std::collections::BTreeMap<String, IndexInfo> =
        std::collections::BTreeMap::new();
    for r in rows {
        let iname: String = r.try_get_by_index(0).unwrap_or_default();
        let col: String = r.try_get_by_index(1).unwrap_or_default();
        let unique: bool = match dialect {
            Dialect::Postgres => r.try_get_by_index::<bool>(2).unwrap_or(false),
            Dialect::Mysql => r
                .try_get_by_index::<i64>(2)
                .map(|v| v == 0)
                .unwrap_or(false),
            Dialect::Sqlite => unreachable!(),
        };
        if iname.is_empty() {
            continue;
        }
        by_name
            .entry(iname.clone())
            .or_insert_with(|| IndexInfo {
                table: table.to_string(),
                columns: Vec::new(),
                unique,
            })
            .columns
            .push(col);
    }
    Ok(by_name.into_values().collect())
}

/// Map a native SQL type string to an OSDL scalar keyword (best effort).
fn map_type(raw: &str) -> &'static str {
    let r = raw.to_ascii_lowercase();
    let r = r.split('(').next().unwrap_or(&r); // drop (n) precision
    match r {
        "text" | "varchar" | "character varying" | "char" | "clob" | "longtext" | "mediumtext"
        | "tinytext" => "string",
        "integer" | "int" | "int4" | "smallint" | "int2" | "mediumint" | "tinyint" => "int",
        "bigint" | "int8" => "bigint",
        "bigserial" | "serial" | "autoincrement" => "bigint",
        "real" | "float" | "float4" | "float8" | "double" | "double precision" => "float",
        "numeric" | "decimal" | "money" => "numeric",
        "boolean" | "bool" => "bool",
        "timestamp" | "timestamptz" | "datetime" => "datetime",
        "date" => "date",
        "uuid" => "uuid",
        "json" | "jsonb" => "json",
        "blob" | "bytea" | "binary" | "varbinary" | "longblob" | "mediumblob" | "tinyblob" => {
            "binary"
        }
        _ => "string",
    }
}

/// Extract `(precision[, scale])` from a SQL type string such as
/// `numeric(18,4)` or `decimal(12)`. Returns `None` when there is no
/// precision clause (e.g. plain `numeric`, `int`).
fn parse_numeric_precision(data_type: &str) -> Option<(u16, Option<u16>)> {
    let open = data_type.find('(')?;
    let close = data_type.rfind(')')?;
    if close <= open {
        return None;
    }
    let inner = &data_type[open + 1..close];
    let mut parts = inner.split(',');
    let p: u16 = parts.next()?.trim().parse().ok()?;
    let s = parts.next().and_then(|s| s.trim().parse::<u16>().ok());
    Some((p, s))
}

/// Render the collected catalog into OSDL source text.
fn render_osdl(tables: &[String], columns: &[ColumnInfo], indexes: &[IndexInfo]) -> String {
    let mut out = String::new();
    writeln!(
        out,
        "# OSDL schema (reverse-engineered from a live database)."
    )
    .ok();
    writeln!(out, "# Review and refine before committing.").ok();

    for table in tables {
        let cols: Vec<&ColumnInfo> = columns.iter().filter(|c| c.table == *table).collect();
        if cols.is_empty() {
            continue;
        }
        // PascalCase the model name from the table (snake/plural -> singular Pascal).
        let model = to_model_name(table);
        writeln!(out).ok();
        writeln!(out, "{model}").ok();

        for c in &cols {
            let mut parts = vec![map_type(&c.data_type).to_string()];
            // Preserve timezone-awareness on round-trip: a tz-aware source column
            // (Postgres `timestamptz` / `timestamp with time zone`) must come back
            // as `datetime -tz`, otherwise `pull` silently drops the tz semantics.
            // SQLite has no tz-aware type (datetime is TEXT), so it stays plain.
            let raw = c.data_type.to_ascii_lowercase();
            let tz_aware = raw.contains("timestamptz") || raw.contains("with time zone");
            if c.is_pk {
                parts.push("-pk".into());
            }
            if c.is_unique && !c.is_pk {
                parts.push("-uniq".into());
            }
            if !c.not_null && !c.is_pk {
                parts.push("-null".into());
            }
            if tz_aware && map_type(&c.data_type) == "datetime" {
                parts.push("-tz".into());
            }
            // Preserve numeric precision/scale on round-trip: e.g. Postgres
            // `numeric(18,4)` or MySQL `decimal(12,2)` must come back as
            // `numeric -precision 18,4` so the exact type is reproduced.
            if let Some((p, s)) = parse_numeric_precision(&c.data_type) {
                if let Some(s) = s {
                    parts.push(format!("-precision {p},{s}"));
                } else {
                    parts.push(format!("-precision {p}"));
                }
            }
            if let Some(d) = &c.default {
                // Only surface simple literal defaults (0, '', now, etc.).
                if is_simple_default(d) {
                    parts.push(format!("-default {d}"));
                }
            }
            // Avoid trailing whitespace when no intents.
            if parts.len() == 1 {
                writeln!(out, "  {} {}", c.name, parts[0]).ok();
            } else {
                writeln!(out, "  {} {}", c.name, parts.join(" ")).ok();
            }
        }

        // Model-level composite indexes (multi-column, or any unique set).
        // No trailing comment here: the OSDL parser does not accept inline
        // comments on intent lines, so the emitted schema must round-trip
        // cleanly through `osdl build`.
        let tbl_indexes: Vec<&IndexInfo> = indexes.iter().filter(|i| i.table == *table).collect();
        for idx in tbl_indexes {
            let fields = idx.columns.to_vec().join(",");
            if idx.unique {
                writeln!(out, "  -uniq {fields}").ok();
            } else {
                writeln!(out, "  -index {fields}").ok();
            }
        }
    }

    // Always end with a newline.
    writeln!(out).ok();
    out
}

/// Heuristic: only emit `-default` for literal-ish values, not expressions.
fn is_simple_default(d: &str) -> bool {
    let t = d.trim();
    if t.is_empty() {
        return false;
    }
    // Drop wrapped parens (e.g. (now())).
    let t = t.trim_matches(|c| c == '(' || c == ')');
    t.parse::<i64>().is_ok()
        || t.parse::<f64>().is_ok()
        || (t.starts_with('\'') && t.ends_with('\''))
        || matches!(
            t.to_ascii_lowercase().as_str(),
            "now"
                | "now()"
                | "current_timestamp"
                | "true"
                | "false"
                | "uuid_generate_v4()"
                | "gen_random_uuid()"
        )
}

/// Validate that an identifier (table/index name from the catalog) is a safe
/// SQL identifier before interpolating it into a PRAGMA statement, which
/// cannot take bound parameters. Rejects anything with non-identifier chars.
fn sanitize_ident(name: &str) -> Result<String, String> {
    if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(name.to_string())
    } else {
        Err(format!("refusing unsafe identifier: {name:?}"))
    }
}

/// Convert a table name to a singular PascalCase OSDL model name.
fn to_model_name(table: &str) -> String {
    // Strip a trailing 's' for the common plural case, then PascalCase.
    let singular = table.trim_end_matches(['s', '_']);
    let mut out = String::new();
    let mut upper = true;
    for c in singular.chars() {
        if c == '_' || c == '-' || c == ' ' {
            upper = true;
            continue;
        }
        if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    if out.is_empty() {
        out = singular.to_string();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_mapping_common() {
        assert_eq!(map_type("varchar(255)"), "string");
        assert_eq!(map_type("INTEGER"), "int");
        assert_eq!(map_type("timestamptz"), "datetime");
        assert_eq!(map_type("jsonb"), "json");
        assert_eq!(map_type("bytea"), "binary");
    }

    #[test]
    fn type_mapping_preserves_numeric_precision() {
        // numeric / decimal / money must map to `numeric`, NOT `float` —
        // collapsing them to float loses precision on round-trip.
        assert_eq!(map_type("numeric"), "numeric");
        assert_eq!(map_type("decimal"), "numeric");
        assert_eq!(map_type("numeric(18,4)"), "numeric");
        assert_eq!(map_type("money"), "numeric");
        // floats stay floats
        assert_eq!(map_type("double precision"), "float");
        assert_eq!(map_type("real"), "float");
    }

    #[test]
    fn render_osdl_preserves_tz_on_timestamptz() {
        let tables = vec!["users".to_string()];
        let columns = vec![ColumnInfo {
            table: "users".to_string(),
            name: "created_at".to_string(),
            data_type: "timestamptz".to_string(),
            not_null: true,
            is_pk: false,
            is_unique: false,
            default: None,
        }];
        let out = render_osdl(&tables, &columns, &[]);
        assert!(
            out.contains("created_at datetime -tz"),
            "timestamptz should round-trip as `datetime -tz`, got:\n{out}"
        );
    }

    #[test]
    fn render_osdl_no_tz_for_plain_timestamp() {
        let tables = vec!["users".to_string()];
        let columns = vec![ColumnInfo {
            table: "users".to_string(),
            name: "created_at".to_string(),
            data_type: "timestamp".to_string(),
            not_null: true,
            is_pk: false,
            is_unique: false,
            default: None,
        }];
        let out = render_osdl(&tables, &columns, &[]);
        assert!(
            out.contains("created_at datetime"),
            "plain timestamp should round-trip as `datetime`, got:\n{out}"
        );
        assert!(
            !out.contains("-tz"),
            "plain timestamp must NOT get a -tz flag, got:\n{out}"
        );
    }

    #[test]
    fn model_naming_singularises() {
        assert_eq!(to_model_name("users"), "User");
        assert_eq!(to_model_name("blog_posts"), "BlogPost");
        assert_eq!(to_model_name("account"), "Account");
    }

    #[test]
    fn simple_default_guards() {
        assert!(is_simple_default("0"));
        assert!(is_simple_default("'active'"));
        assert!(is_simple_default("now()"));
        assert!(!is_simple_default("(some_func(a, b))"));
    }

    #[test]
    fn sanitize_rejects_unsafe() {
        assert!(sanitize_ident("good_name").is_ok());
        assert!(sanitize_ident("evil'); DROP--").is_err());
    }
}
