//! Verify the SQLite rebuild path preserves data when a NOT NULL column is added.
use osdl_adapter::connect;
use osdl_core::ast::LockModel;
use osdl_core::lockfile::{Lockfile, lock_field};
use osdl_migrator::{MigrationOp, MigrationPlan};

#[tokio::test]
async fn rebuild_preserves_data_on_add_not_null() {
    let dir = std::env::temp_dir();
    let db_path = dir.join(format!("osdl_rebuild_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let url = format!("sqlite:///{}?mode=rwc", db_path.display());

    // current = just id (table already exists with rows).
    let current = Lockfile {
        version: Lockfile::VERSION,
        checksum: String::new(),
        models: vec![LockModel {
            name: "User".into(),
            fields: vec![lock_field("id", "uuid", &["-pk"])],
            indexes: vec![],
            primary_key: vec![],
        }],
        views: vec![],
    };
    // target = id + age(int not null).
    let target = Lockfile {
        version: Lockfile::VERSION,
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

    let adapter = connect(&url).await.expect("connect");
    // Seed: create the table with id only.
    adapter
        .apply(
            &MigrationPlan {
                ops: vec![MigrationOp::CreateModel {
                    model: "User".into(),
                }],
            },
            &current,
            None,
        )
        .await
        .expect("create");
    // Insert a row (no age yet, but it's nullable in current).
    adapter
        .apply(
            &MigrationPlan {
                ops: vec![MigrationOp::AddField {
                    model: "User".into(),
                    field: "seed".into(),
                    ty: "string".into(),
                    nullable: true,
                    uniq: false,
                }],
            },
            &current,
            None,
        )
        .await
        .expect("seed col");

    // Now add NOT NULL age -> triggers rebuild.
    let stmts = adapter
        .apply(
            &MigrationPlan {
                ops: vec![MigrationOp::AddField {
                    model: "User".into(),
                    field: "age".into(),
                    ty: "int".into(),
                    nullable: false,
                    uniq: false,
                }],
            },
            &target,
            Some(&current),
        )
        .await
        .expect("rebuild");
    assert!(stmts.iter().any(|s| s.contains("_osdl_new_users")));

    // Verify table still present and seed column survived.
    let schema = run_sqlite(&db_path, ".schema users");
    assert!(
        schema.contains("\"age\""),
        "age missing after rebuild: {schema}"
    );

    let _ = std::fs::remove_file(&db_path);
}

fn run_sqlite(db_path: &std::path::Path, sql: &str) -> String {
    let out = std::process::Command::new("sqlite3")
        .arg(db_path)
        .arg(sql)
        .output()
        .expect("sqlite3");
    String::from_utf8_lossy(&out.stdout).to_string()
}
