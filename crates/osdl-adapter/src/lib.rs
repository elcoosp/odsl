//! Live database adapters for the OSDL compiler.
//!
//! The migrator produces a backend-agnostic [`MigrationPlan`]; this crate
//! turns that plan into real DDL executed against a live database:
//!
//! * [`SqlAdapter`] — SQLite / Postgres via SeaORM (`execute_unprepared`).
//! * [`MongoAdapter`] — MongoDB collections + `$jsonSchema` validators via the
//!   official driver.
//!
//! A [`connect`](connect) factory picks the backend from the connection URL
//! scheme (`sqlite://`, `postgres://`, `mongodb://`) and returns a boxed
//! [`SchemaAdapter`].

#![allow(clippy::result_large_err)]

pub mod error;
pub mod introspect;
pub mod migrate;
pub mod mongo;
pub mod naming;
pub mod sql;

pub use error::AdapterError;

use osdl_core::lockfile::Lockfile;
use osdl_migrator::MigrationPlan;
use sea_orm::ConnectionTrait;

/// The physical backend an adapter talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Sqlite,
    Postgres,
    Mysql,
    Mongo,
}

/// A backend that can apply a [`MigrationPlan`] against a live database.
#[async_trait::async_trait]
pub trait SchemaAdapter: Send + Sync {
    /// Which backend this adapter drives.
    fn backend(&self) -> Backend;

    /// Apply every op in `plan` against the live database, using `target` (the
    /// desired lockfile state) to materialize complete `CREATE TABLE` /
    /// validator documents. `current` is the prior lockfile state (the baseline
    /// being migrated from); it is required for SQLite table rebuilds that
    /// preserve data when a column is added non-null, dropped, or altered.
    /// Returns the SQL statements / Mongo commands that were executed, in
    /// order, for logging/audit.
    async fn apply(
        &self,
        plan: &MigrationPlan,
        target: &Lockfile,
        current: Option<&Lockfile>,
    ) -> Result<Vec<String>, AdapterError>;

    /// Apply the *rollback* (down) plan: revert the live database from `target`
    /// back to `current`. `plan` is the forward plan (`MigrationPlan::diff(
    /// target, current)`); each op is mapped to its inverse statement/command
    /// inside the adapter. For SQL this executes the reversed DDL; for Mongo it
    /// drops created collections / removes added fields / re-emits the prior
    /// validator for dropped/altered fields. Returns the statements/commands
    /// executed, in order, for logging/audit.
    async fn revert(
        &self,
        plan: &MigrationPlan,
        target: &Lockfile,
        current: Option<&Lockfile>,
    ) -> Result<Vec<String>, AdapterError>;

    /// Create the idempotency history table (`_osdl_migrations`) if absent.
    async fn ensure_history_table(&self) -> Result<(), AdapterError> {
        Ok(())
    }

    /// Record a successfully-applied migration for idempotency. Default no-op
    /// (Mongo and other backends manage history separately).
    async fn record_applied(&self, _name: &str, _checksum: &str) -> Result<(), AdapterError> {
        Ok(())
    }

    /// Names of migrations already applied (for idempotency checks). Default
    /// empty (backends without a tracker always re-apply).
    async fn applied_migrations(&self) -> Result<Vec<String>, AdapterError> {
        Ok(Vec::new())
    }
}

/// Connect to a live database described by `db_url` and return the matching
/// adapter. The backend is chosen from the URL scheme.
pub async fn connect(db_url: &str) -> Result<Box<dyn SchemaAdapter>, AdapterError> {
    if let Some(dialect) = sql::SqlDialect::from_url(db_url) {
        let backend = match dialect {
            sql::SqlDialect::Sqlite => Backend::Sqlite,
            sql::SqlDialect::Postgres => Backend::Postgres,
            sql::SqlDialect::Mysql => Backend::Mysql,
        };
        let conn = sea_orm::Database::connect(db_url)
            .await
            .map_err(|e| AdapterError::Connect(e.to_string()))?;
        Ok(Box::new(SqlAdapter {
            conn,
            dialect,
            backend,
        }))
    } else if db_url.starts_with("mongodb://") || db_url.starts_with("mongodb+srv://") {
        let client = mongodb::Client::with_uri_str(db_url)
            .await
            .map_err(|e| AdapterError::Connect(e.to_string()))?;
        let db_name = db_name_from_url(db_url);
        let db = client.database(&db_name);
        Ok(Box::new(MongoAdapter { db }))
    } else {
        Err(AdapterError::Connect(format!(
            "unsupported database URL scheme: {db_url}"
        )))
    }
}

/// Extract the database name from a MongoDB URL (the path segment after the
/// host). Falls back to `osdl` when absent.
fn db_name_from_url(url: &str) -> String {
    let after_scheme = url
        .trim_start_matches("mongodb+srv://")
        .trim_start_matches("mongodb://");
    // strip credentials/host
    let path = after_scheme.split(['/', '?']).nth(1).unwrap_or("");
    let name = path.trim_matches('/');
    if name.is_empty() {
        "osdl".to_string()
    } else {
        name.to_string()
    }
}

/// SeaORM-backed SQL adapter (SQLite or Postgres).
pub struct SqlAdapter {
    conn: sea_orm::DatabaseConnection,
    dialect: sql::SqlDialect,
    backend: Backend,
}

impl SqlAdapter {
    /// Idempotency tracker table. Records the name + checksum of every
    /// migration that has been applied, so re-running `up` on an already
    /// migrated database is a no-op for already-applied migrations.
    const HISTORY_TABLE: &'static str = "_osdl_migrations";

    /// Create the history table if it does not already exist.
    pub async fn ensure_history_table(&self) -> Result<(), AdapterError> {
        let stmt = format!(
            "CREATE TABLE IF NOT EXISTS {} (\
                name TEXT PRIMARY KEY, \
                checksum TEXT NOT NULL, \
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))\
            )",
            Self::HISTORY_TABLE
        );
        self.conn
            .execute_unprepared(&stmt)
            .await
            .map_err(|e| AdapterError::Exec(stmt.clone() + &e.to_string()))?;
        Ok(())
    }

    /// Names of migrations already recorded in the history table.
    pub async fn applied_migrations(&self) -> Result<Vec<String>, AdapterError> {
        use sea_orm::sea_query::{Alias, Expr, Order, Query};
        let select = Query::select()
            .expr(Expr::col(Alias::new("name")))
            .from(Alias::new(Self::HISTORY_TABLE))
            .order_by(Alias::new("name"), Order::Asc)
            .to_owned();
        let rows = self
            .conn
            .query_all(&select)
            .await
            .map_err(|e| AdapterError::Exec(e.to_string()))?;
        Ok(rows
            .into_iter()
            .filter_map(|row| row.try_get("", "name").ok())
            .collect())
    }

    /// Record that a migration was applied (idempotent: ignores duplicates).
    pub async fn record_applied(&self, name: &str, checksum: &str) -> Result<(), AdapterError> {
        let stmt = format!(
            "INSERT OR IGNORE INTO {} (name, checksum) VALUES ('{}', '{}')",
            Self::HISTORY_TABLE,
            name.replace('\'', "''"),
            checksum.replace('\'', "''")
        );
        self.conn
            .execute_unprepared(&stmt)
            .await
            .map_err(|e| AdapterError::Exec(stmt.clone() + &e.to_string()))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl SchemaAdapter for SqlAdapter {
    fn backend(&self) -> Backend {
        self.backend
    }

    async fn ensure_history_table(&self) -> Result<(), AdapterError> {
        SqlAdapter::ensure_history_table(self).await
    }

    async fn record_applied(&self, name: &str, checksum: &str) -> Result<(), AdapterError> {
        SqlAdapter::record_applied(self, name, checksum).await
    }

    async fn applied_migrations(&self) -> Result<Vec<String>, AdapterError> {
        SqlAdapter::applied_migrations(self).await
    }

    async fn apply(
        &self,
        plan: &MigrationPlan,
        target: &Lockfile,
        current: Option<&Lockfile>,
    ) -> Result<Vec<String>, AdapterError> {
        let mut applied = Vec::new();
        for op in &plan.ops {
            for stmt in sql::op_to_sql(self.dialect, op, target, current) {
                // Skip informational comments (SQLite ALTER COLUMN no-op).
                if stmt.trim_start().starts_with("--") {
                    applied.push(stmt);
                    continue;
                }
                self.conn
                    .execute_unprepared(&stmt)
                    .await
                    .map_err(|e| AdapterError::Exec(format!("{}: {}", stmt, e)))?;
                tracing::info!(sql = %stmt, "executed DDL");
                applied.push(stmt);
            }
        }
        Ok(applied)
    }

    async fn revert(
        &self,
        plan: &MigrationPlan,
        target: &Lockfile,
        current: Option<&Lockfile>,
    ) -> Result<Vec<String>, AdapterError> {
        let mut applied: Vec<String> = Vec::new();
        // Inverse op order with inverse statements (drop what `up` created, etc.).
        for stmt in migrate::render_down_sql(self.dialect, plan, target, current) {
            // Skip informational comments (backend-specific ALTER COLUMN guards).
            if stmt.trim_start().starts_with("--") {
                applied.push(stmt);
                continue;
            }
            self.conn
                .execute_unprepared(&stmt)
                .await
                .map_err(|e| AdapterError::Exec(format!("{}: {}", stmt, e)))?;
            tracing::info!(sql = %stmt, "executed rollback DDL");
            applied.push(stmt);
        }
        Ok(applied)
    }
}

/// MongoDB adapter.
pub struct MongoAdapter {
    db: mongodb::Database,
}

#[async_trait::async_trait]
impl SchemaAdapter for MongoAdapter {
    fn backend(&self) -> Backend {
        Backend::Mongo
    }

    async fn apply(
        &self,
        plan: &MigrationPlan,
        target: &Lockfile,
        _current: Option<&Lockfile>,
    ) -> Result<Vec<String>, AdapterError> {
        let mut applied = Vec::new();
        for op in &plan.ops {
            let mongo_ops = mongo::op_to_mongo(op, target);
            for mop in &mongo_ops {
                let desc = format!("{mop:?}");
                mongo::apply_ops(&self.db, std::slice::from_ref(mop)).await?;
                applied.push(desc);
            }
        }
        Ok(applied)
    }

    async fn revert(
        &self,
        plan: &MigrationPlan,
        _target: &Lockfile,
        current: Option<&Lockfile>,
    ) -> Result<Vec<String>, AdapterError> {
        // Fall back to an empty baseline when no prior lockfile is supplied.
        let empty = Lockfile {
            version: Lockfile::VERSION,
            checksum: String::new(),
            models: vec![],
        };
        let current = current.unwrap_or(&empty);
        let mut applied = Vec::new();
        for op in &plan.ops {
            let mongo_ops = mongo::op_to_mongo_down(op, current);
            for mop in &mongo_ops {
                let desc = format!("{mop:?}");
                mongo::apply_ops(&self.db, std::slice::from_ref(mop)).await?;
                applied.push(desc);
            }
        }
        Ok(applied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_sqlite() {
        assert_eq!(
            sql::SqlDialect::from_url("sqlite://foo.db"),
            Some(sql::SqlDialect::Sqlite)
        );
        assert_eq!(
            sql::SqlDialect::from_url("postgres://localhost/x"),
            Some(sql::SqlDialect::Postgres)
        );
        assert_eq!(sql::SqlDialect::from_url("mongodb://localhost/x"), None);
    }

    #[test]
    fn mongo_db_name_parsing() {
        assert_eq!(db_name_from_url("mongodb://localhost:27017/mydb"), "mydb");
        assert_eq!(db_name_from_url("mongodb://localhost:27017/"), "osdl");
        assert_eq!(
            db_name_from_url("mongodb+srv://u:p@cluster.example.com/shop?retryWrites=true"),
            "shop"
        );
    }
}
