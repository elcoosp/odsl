//! Integration tests that exercise the `osdl` binary end-to-end (the
//! `CARGO_BIN_EXE_osdl` env var is only populated for integration tests).

use std::process::Command;

fn osdl() -> Command {
    Command::new(env!("CARGO_BIN_EXE_osdl"))
}

#[test]
fn migrate_down_prints_rollback_sql_without_db() {
    let dir = std::env::temp_dir().join(format!("osdl-down-it-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);

    // Deploy `User`+`Comment` via `up` (this also writes the JSON osdl.lock).
    let full = dir.join("schema.osdl");
    let _ = std::fs::write(
        &full,
        "User\n  id uuid -pk\n  email string -uniq\nComment\n  id uuid -pk\n  body string\n",
    );
    // `up` needs no DB to compute+write the lock (it only touches the DB when
    // --db-url is given). Run it without --db-url so osdl.lock is written.
    let up = osdl()
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
        dir.join("osdl.lock").exists(),
        "migrate up should have written osdl.lock"
    );

    // Desired schema without `Comment` -> the rollback should DROP it.
    let desired = dir.join("nocomment.osdl");
    let _ = std::fs::write(&desired, "User\n  id uuid -pk\n  email string -uniq\n");
    let out = osdl()
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
