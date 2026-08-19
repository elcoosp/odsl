//! Integration tests that exercise the `odsl` binary end-to-end (the
//! `CARGO_BIN_EXE_odsl` env var is only populated for integration tests).

use std::process::Command;

fn odsl() -> Command {
    Command::new(env!("CARGO_BIN_EXE_odsl"))
}

#[test]
fn migrate_down_prints_rollback_sql_without_db() {
    let dir = std::env::temp_dir().join(format!("odsl-down-it-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);

    // Deploy `User`+`Comment` via `up` (this also writes the JSON odsl.lock).
    let full = dir.join("schema.odsl");
    let _ = std::fs::write(
        &full,
        "User\n  id uuid -pk\n  email string -uniq\nComment\n  id uuid -pk\n  body string\n",
    );
    // `up` needs no DB to compute+write the lock (it only touches the DB when
    // --db-url is given). Run it without --db-url so odsl.lock is written.
    let up = odsl()
        .arg("migrate")
        .arg("up")
        .arg(&full)
        .arg("--target")
        .arg("sea-orm-sqlite")
        .output()
        .expect("run migrate up");
    assert!(
        up.status.success(),
        "migrate up failed: {}",
        String::from_utf8_lossy(&up.stderr)
    );
    assert!(
        dir.join("odsl.lock").exists(),
        "migrate up should have written odsl.lock"
    );

    // Desired schema without `Comment` -> the rollback should DROP it.
    let desired = dir.join("nocomment.odsl");
    let _ = std::fs::write(&desired, "User\n  id uuid -pk\n  email string -uniq\n");
    let out = odsl()
        .arg("migrate")
        .arg("down")
        .arg(&desired)
        .arg("--target")
        .arg("sea-orm-sqlite")
        .output()
        .expect("run migrate down");
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("DROP TABLE") || stdout.contains("rollback"),
        "expected rollback SQL, got:\n{stdout}"
    );
    // And it must name the `Comments` table (the deployable name for `Comment`).
    assert!(
        stdout.to_lowercase().contains("comments"),
        "rollback should target the comments table, got:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
