//! Integration tests that execute migrations against a real SQLite database.
//!
//! These hit the `sqlite3` CLI to assert the resulting schema, proving the
//! adapter emits DDL that a live SQL engine actually accepts.

use osdl_adapter::connect;
use osdl_core::ast::LockModel;
use osdl_core::lockfile::{Lockfile, lock_field};
use osdl_migrator::{MigrationOp, MigrationPlan};

#[tokio::test]
async fn applies_create_and_add_field_to_live_sqlite() {
    let dir = std::env::temp_dir();
    let db_path = dir.join(format!("osdl_test_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let url = format!("sqlite:///{}?mode=rwc", db_path.display());

    // Target schema: User(id uuid -pk, email string -uniq, age int -null)
    let target = Lockfile {
        version: Lockfile::VERSION,
        checksum: String::new(),
        models: vec![LockModel {
            name: "User".into(),
            fields: vec![
                lock_field("id", "uuid", &["-pk"]),
                lock_field("email", "string", &["-uniq"]),
                lock_field("age", "int", &["-null"]),
            ],
            indexes: vec![],
        }],
    };

    // Plan = create the model.
    let plan = MigrationPlan {
        ops: vec![MigrationOp::CreateModel {
            model: "User".into(),
        }],
    };

    let adapter = connect(&url).await.expect("connect sqlite");
    let applied = adapter
        .apply(&plan, &target, None)
        .await
        .expect("apply plan");
    assert_eq!(applied.len(), 1);
    assert!(applied[0].starts_with("CREATE TABLE"));

    // Assert the table + columns exist in the live DB.
    let schema = run_sqlite(&db_path, ".schema users");
    assert!(
        schema.contains("CREATE TABLE"),
        "table users missing: {schema}"
    );
    assert!(schema.contains("\"id\""), "id column missing");
    assert!(schema.contains("\"email\""), "email column missing");
    assert!(schema.contains("\"age\""), "age column missing");

    // Now add a field via ALTER.
    let plan2 = MigrationPlan {
        ops: vec![MigrationOp::AddField {
            model: "User".into(),
            field: "name".into(),
            ty: "string".into(),
            nullable: false,
            uniq: false,
        }],
    };
    let _ = adapter
        .apply(&plan2, &target, None)
        .await
        .expect("apply add field");
    let schema2 = run_sqlite(&db_path, ".schema users");
    assert!(
        schema2.contains("\"name\""),
        "name column not added: {schema2}"
    );

    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn applies_reference_as_foreign_key() {
    let dir = std::env::temp_dir();
    let db_path = dir.join(format!("osdl_test_fk_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let url = format!("sqlite:///{}?mode=rwc", db_path.display());

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
        ops: vec![
            MigrationOp::CreateModel {
                model: "User".into(),
            },
            MigrationOp::CreateModel {
                model: "Post".into(),
            },
        ],
    };
    let adapter = connect(&url).await.expect("connect");
    adapter.apply(&plan, &target, None).await.expect("apply");
    let schema = run_sqlite(&db_path, ".schema posts");
    assert!(
        schema.contains("REFERENCES \"users\"(\"id\")"),
        "FK not emitted: {schema}"
    );
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn history_tracker_records_and_detects_applied() {
    let dir = std::env::temp_dir();
    let db_path = dir.join(format!("osdl_hist_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let url = format!("sqlite:///{}?mode=rwc", db_path.display());

    let adapter = connect(&url).await.expect("connect");
    adapter
        .ensure_history_table()
        .await
        .expect("ensure history");
    // Empty initially.
    assert!(adapter.applied_migrations().await.unwrap().is_empty());
    // Record two.
    adapter
        .record_applied("m20240101_a", "c1")
        .await
        .expect("record a");
    adapter
        .record_applied("m20240102_b", "c2")
        .await
        .expect("record b");
    // Recorded migrations are visible and idempotent (no error on dup).
    let applied = adapter.applied_migrations().await.unwrap();
    assert_eq!(applied.len(), 2);
    assert!(applied.contains(&"m20240101_a".to_string()));
    adapter
        .record_applied("m20240101_a", "c1")
        .await
        .expect("re-record a is idempotent");
    assert_eq!(adapter.applied_migrations().await.unwrap().len(), 2);

    // History table persists in the live DB.
    let schema = run_sqlite(&db_path, ".schema _osdl_migrations");
    assert!(schema.contains("_osdl_migrations"), "history table missing");

    let _ = std::fs::remove_file(&db_path);
}

fn run_sqlite(db_path: &std::path::Path, sql: &str) -> String {
    let out = std::process::Command::new("sqlite3")
        .arg(db_path)
        .arg(sql)
        .output()
        .expect("sqlite3 available");
    String::from_utf8_lossy(&out.stdout).to_string()
}
