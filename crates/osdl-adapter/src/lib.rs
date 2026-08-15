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
    Mongo,
}

/// A backend that can apply a [`MigrationPlan`] against a live database.
#[async_trait::async_trait]
pub trait SchemaAdapter: Send + Sync {
    /// Which backend this adapter drives.
    fn backend(&self) -> Backend;

    /// Apply every op in `plan` against the live database, using `target` (the
    /// desired lockfile state) to materialize complete `CREATE TABLE` /
    /// validator documents. Returns the SQL statements / Mongo commands that
    /// were executed, in order, for logging/audit.
    async fn apply(
        &self,
        plan: &MigrationPlan,
        target: &Lockfile,
    ) -> Result<Vec<String>, AdapterError>;
}

/// Connect to a live database described by `db_url` and return the matching
/// adapter. The backend is chosen from the URL scheme.
pub async fn connect(db_url: &str) -> Result<Box<dyn SchemaAdapter>, AdapterError> {
    if let Some(dialect) = sql::SqlDialect::from_url(db_url) {
        let backend = match dialect {
            sql::SqlDialect::Sqlite => Backend::Sqlite,
            sql::SqlDialect::Postgres => Backend::Postgres,
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

#[async_trait::async_trait]
impl SchemaAdapter for SqlAdapter {
    fn backend(&self) -> Backend {
        self.backend
    }

    async fn apply(
        &self,
        plan: &MigrationPlan,
        target: &Lockfile,
    ) -> Result<Vec<String>, AdapterError> {
        let mut applied = Vec::new();
        for op in &plan.ops {
            for stmt in sql::op_to_sql(self.dialect, op, target) {
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
