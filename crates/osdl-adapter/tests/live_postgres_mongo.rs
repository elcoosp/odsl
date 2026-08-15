//! Live integration tests that apply OSDL migration plans against real
//! Postgres and MongoDB instances spun up via `testcontainers`.
//!
//! These require a working Docker daemon. When Docker is unavailable the tests
//! skip themselves (printing a notice) so the suite stays green in
//! environments without containers — run them in CI or locally with Docker to
//! exercise the real backends.

use osdl_adapter::connect;
use osdl_core::ast::LockModel;
use osdl_core::lockfile::{Lockfile, lock_field};
use osdl_migrator::{MigrationOp, MigrationPlan};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::{mongo::Mongo, postgres::Postgres};

/// Start a Postgres container; return its connection URL, or `None` if Docker
/// is unavailable (in which case the calling test should skip).
async fn start_postgres() -> Option<String> {
    let container = Postgres::default().start().await.ok()?;
    let host = container.get_host().await.ok()?;
    let port = container.get_host_port_ipv4(5432).await.ok()?;
    // testcontainers-modules Postgres defaults: user postgres, password postgres, db postgres.
    Some(format!(
        "postgres://postgres:postgres@{}:{}/postgres",
        host, port
    ))
}

/// Start a Mongo container; return its connection URL, or `None` if Docker is
/// unavailable.
async fn start_mongo() -> Option<String> {
    let container = Mongo::default().start().await.ok()?;
    let host = container.get_host().await.ok()?;
    let port = container.get_host_port_ipv4(27017).await.ok()?;
    Some(format!("mongodb://{}:{}/osdl", host, port))
}

#[tokio::test]
async fn postgres_apply_create_model_live() {
    let Some(url) = start_postgres().await else {
        eprintln!("skipping: Docker / Postgres container unavailable");
        return;
    };

    let target = Lockfile {
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
    };
    let plan = MigrationPlan {
        ops: vec![MigrationOp::CreateModel {
            model: "User".into(),
        }],
    };

    let adapter = connect(&url).await.expect("connect postgres");
    let applied = adapter
        .apply(&plan, &target, None)
        .await
        .expect("apply plan");
    assert_eq!(applied.len(), 1);
    assert!(
        applied[0].contains("CREATE TABLE"),
        "unexpected DDL: {applied:?}"
    );
}

#[tokio::test]
async fn mongo_apply_create_model_live() {
    let Some(url) = start_mongo().await else {
        eprintln!("skipping: Docker / Mongo container unavailable");
        return;
    };

    let target = Lockfile {
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
    };
    let plan = MigrationPlan {
        ops: vec![MigrationOp::CreateModel {
            model: "User".into(),
        }],
    };

    let adapter = connect(&url).await.expect("connect mongo");
    adapter
        .apply(&plan, &target, None)
        .await
        .expect("apply plan");
    // Mongo applies validators; reaching here without error means the
    // collection + validator were created successfully.
}
