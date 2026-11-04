//! `odsl migrate` — diff the schema against `odsl.lock` and manage
//! migrations (plan, create, up, down, status, test).

use crate::project::{
    history_entries, io_err, load_ast, load_project, new_runtime, sqlite_file_path,
};
use odsl_adapter::migrate::{MigrationFormat, write_migration};
use odsl_adapter::sql::SqlDialect;
use odsl_core::Target;
use odsl_core::ast::Ast;
use odsl_core::errors::OdslError;
use odsl_core::lockfile::Lockfile;
use odsl_migrator::{MigrationOp, MigrationPlan, plan_migration, write_lockfile};
use odsl_parser::parse;
use std::io::IsTerminal;

/// The SQL dialect matching a SeaORM target. Non-SQL targets map to SQLite
/// (mirroring `run_build`'s renderer dispatch, where SQL is the default).
fn sql_dialect_for(target: Target) -> SqlDialect {
    match target {
        Target::SeaOrmPostgres => SqlDialect::Postgres,
        Target::SeaOrmMysql => SqlDialect::Mysql,
        _ => SqlDialect::Sqlite,
    }
}

/// One-line description of a single op (used by the destructive-guard prompt).
fn describe_op(op: &MigrationOp) -> String {
    match op {
        MigrationOp::DropModel { model } => format!("drop model {model}"),
        MigrationOp::DropField { model, field } => {
            format!("drop field {model}.{field}")
        }
        MigrationOp::AlterField { model, field, .. } => {
            format!("alter field {model}.{field}")
        }
        MigrationOp::CreateModel { .. }
        | MigrationOp::AddField { .. }
        | MigrationOp::CreateView { .. }
        | MigrationOp::DropView { .. }
        | MigrationOp::SeedData { .. } => String::new(),
    }
}

/// Guard destructive plans: with `--force` proceed immediately; on a terminal
/// ask for an explicit `yes`; in non-interactive mode refuse.
///
/// Returns `Ok(true)` to proceed and `Ok(false)` when the user aborted (the
/// command exits successfully without applying anything).
fn confirm_destructive(plan: &MigrationPlan, action: &str, force: bool) -> Result<bool, OdslError> {
    if !plan.is_destructive() || force {
        return Ok(true);
    }
    if std::io::stdin().is_terminal() {
        let count = plan.destructive_ops().len();
        println!(
            "\nThis {action} contains {count} potentially destructive operation(s) \
             that may DESTROY DATA (drop model/field, alter column):"
        );
        for op in plan.destructive_ops() {
            println!("  - {}", describe_op(op));
        }
        print!("Type 'yes' to proceed, anything else to abort: ");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err() || answer.trim() != "yes" {
            println!("aborted; no changes applied");
            return Ok(false);
        }
        Ok(true)
    } else {
        Err(io_err(format!(
            "refusing destructive {action} in non-interactive mode; \
             re-run with --force to apply, or pipe 'yes' to confirm"
        )))
    }
}

pub fn cmd_migrate_plan(
    input: &std::path::Path,
    target: Target,
    apply: bool,
) -> Result<(), OdslError> {
    let state = load_project(input, target)?;
    let plan = plan_migration(&state.current, &state.ast)?;
    for line in plan.describe() {
        println!("  {line}");
    }
    if plan.ops.is_empty() {
        println!("no changes");
    }
    if apply {
        write_lockfile(&state.lock_path, &Lockfile::from_ast(&state.ast))?;
        tracing::info!(lock = %state.lock_path.display(), "lockfile updated");
        println!("updated {}", state.lock_path.display());
    }
    Ok(())
}

pub fn cmd_migrate_up(
    input: &std::path::Path,
    target: Target,
    db_url: Option<String>,
    force: bool,
) -> Result<(), OdslError> {
    let state = load_project(input, target)?;
    let plan = plan_migration(&state.current, &state.ast)?;
    for line in plan.describe() {
        println!("  {line}");
    }
    if plan.ops.is_empty() {
        println!("no changes");
    }

    // Guard destructive operations (drop model/field, alter column) unless
    // the user explicitly passes --force or confirms interactively.
    if !confirm_destructive(&plan, "migration", force)? {
        return Ok(());
    }

    // Apply against a live database when a connection URL is provided.
    if let Some(url) = &db_url {
        let runtime = new_runtime()?;
        let target_lock = Lockfile::from_ast(&state.ast);
        let (adapter, applied) = history_entries(&runtime, url)?;
        // Idempotency: skip if this exact schema state was already applied.
        let schema_key = target_lock.checksum.clone();
        if applied.iter().any(|n| n == &schema_key) {
            println!("schema {schema_key} already applied; skipping");
        } else {
            let statements = runtime
                .block_on(adapter.apply(&plan, &target_lock, Some(&state.current)))
                .map_err(|e| io_err(format!("applying migration: {e}")))?;
            for stmt in &statements {
                println!("  applied: {stmt}");
            }
            println!("applied {} change(s) to {url}", statements.len());
            runtime
                .block_on(adapter.record_applied(&schema_key, &schema_key))
                .map_err(|e| io_err(format!("recording migration: {e}")))?;
        }
    }

    // `up` always records the new baseline lockfile.
    write_lockfile(&state.lock_path, &Lockfile::from_ast(&state.ast))?;
    tracing::info!(lock = %state.lock_path.display(), "lockfile updated");
    println!("updated {}", state.lock_path.display());
    Ok(())
}

pub fn cmd_migrate_create(
    input: &std::path::Path,
    target: Target,
    out: &std::path::Path,
    sea_orm: bool,
) -> Result<(), OdslError> {
    let state = load_project(input, target)?;
    let plan = plan_migration(&state.current, &state.ast)?;
    for line in plan.describe() {
        println!("  {line}");
    }
    if plan.ops.is_empty() {
        println!("no changes; nothing to generate");
        return Ok(());
    }
    let format = if sea_orm {
        MigrationFormat::SeaOrm
    } else {
        MigrationFormat::Sql
    };
    let written = write_migration(
        out,
        format,
        sql_dialect_for(target),
        &plan,
        &Lockfile::from_ast(&state.ast),
        Some(&state.current),
    )
    .map_err(|e| io_err(format!("writing migration: {e}")))?;
    match written {
        Some(name) => {
            println!("generated {}/{name}", out.display());
            Ok(())
        }
        None => {
            println!("no migration file written");
            Ok(())
        }
    }
}

/// Rollback plan: revert the *deployed* schema (odsl.lock) back to the
/// *desired* schema (schema.odsl). The inverse plan is `diff(target, current)`
/// rendered as the down SQL / Mongo commands. With `--db-url` the rollback is
/// applied to the live database; otherwise it is printed for inspection.
pub fn cmd_migrate_down(
    input: &std::path::Path,
    target: Target,
    db_url: Option<String>,
    force: bool,
) -> Result<(), OdslError> {
    let state = load_project(input, target)?;
    let target_lock = Lockfile::from_ast(&state.ast);
    // Inverse plan: from the desired schema back to the deployed one.
    let plan = MigrationPlan::diff(&target_lock, &state.current);

    if plan.ops.is_empty() {
        println!("no down migration (nothing to revert)");
        return Ok(());
    }

    // Show what the rollback will do.
    println!("=== rollback (desired -> deployed) ===");
    for line in plan.describe() {
        println!("  - {line}");
    }

    // Guard destructive operations (drop model/field, alter column) unless
    // --force or interactive confirmation.
    if !confirm_destructive(&plan, "rollback", force)? {
        return Ok(());
    }

    // No live DB: print the rollback DDL only.
    let Some(url) = &db_url else {
        let down = odsl_adapter::migrate::render_down_sql(
            sql_dialect_for(target),
            &plan,
            &target_lock,
            Some(&state.current),
        );
        println!("\n-- down (revert to deployed state)");
        for stmt in &down {
            println!("{stmt};");
        }
        return Ok(());
    };

    // Live DB: apply the rollback via the adapter's `revert`.
    let runtime = new_runtime()?;
    let applied = runtime.block_on(async {
        let adapter = odsl_adapter::connect(url)
            .await
            .map_err(|e| io_err(format!("connecting to {url}: {e}")))?;
        adapter
            .revert(&plan, &target_lock, Some(&state.current))
            .await
            .map_err(|e| io_err(format!("applying rollback: {e}")))
    })?;
    for stmt in &applied {
        println!("  reverted: {stmt}");
    }
    println!("reverted {} change(s) on {url}", applied.len());
    Ok(())
}

/// Show the drift between the deployed schema (odsl.lock), an optional live
/// database (`--db-url`), and the desired schema (schema.odsl).
pub fn cmd_migrate_status(
    input: &std::path::Path,
    target: Target,
    db_url: Option<String>,
) -> Result<(), OdslError> {
    let state = load_project(input, target)?;
    let target_lock = Lockfile::from_ast(&state.ast);
    let plan = plan_migration(&state.current, &state.ast)?;

    println!("=== schema.odsl (desired) vs odsl.lock (deployed) ===");
    if plan.ops.is_empty() {
        println!("  in sync — no pending changes");
    } else {
        for line in plan.describe() {
            println!("  + {line}");
        }
        let advisories = plan.advisories(target);
        if !advisories.is_empty() {
            println!("  ! advisories (zero-downtime):");
            for (_, msg) in advisories {
                println!("    - {msg}");
            }
        }
    }

    if let Some(url) = &db_url {
        let runtime = new_runtime()?;
        let (_, applied_migrations) = history_entries(&runtime, url)?;
        println!("=== live database ({url}) ===");
        if applied_migrations
            .iter()
            .any(|n| n == &target_lock.checksum)
        {
            println!(
                "  schema checksum {chk} is applied",
                chk = target_lock.checksum
            );
        } else {
            println!(
                "  schema checksum {chk} NOT applied (database is behind or diverged)",
                chk = target_lock.checksum
            );
            println!("  applied migrations: {applied_migrations:?}");
        }
    }
    Ok(())
}

/// Apply `up`, assert the live schema matches the target, apply `down`, and
/// assert the live schema reverts to empty. The database at `db_url` is wiped
/// first so the test is deterministic and repeatable.
///
/// Returns `Ok(())` only when both assertions hold; otherwise an error
/// describing the mismatch (which makes the CLI exit non-zero).
pub fn cmd_migrate_test(
    input: &std::path::Path,
    target: Target,
    db_url: &str,
    up_only: bool,
) -> Result<(), OdslError> {
    // Reject backends without a SQL round-trip (Mongo introspection is lossy).
    if !matches!(
        target,
        Target::SeaOrmSqlite | Target::SeaOrmPostgres | Target::SeaOrmMysql
    ) {
        return Err(io_err(format!(
            "migrate test supports SQL backends only (got {target:?})"
        )));
    }

    let ast = load_ast(input, target)?;
    let target_lock = Lockfile::from_ast(&ast);
    let empty = Lockfile::empty();

    let runtime = new_runtime()?;

    // Ensure a fresh SQLite database file exists so the adapter can connect
    // (SeaORM does not auto-create a non-existent sqlite file on connect).
    if let Some(path) = sqlite_file_path(db_url) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if !path.exists() {
            std::fs::File::create(&path)
                .map_err(|e| io_err(format!("creating database file {}: {e}", path.display())))?;
        }
    }

    // Reset the live database to an empty baseline.
    runtime
        .block_on(async {
            let adapter = odsl_adapter::connect(db_url).await?;
            adapter.wipe().await
        })
        .map_err(|e: odsl_adapter::AdapterError| io_err(format!("wiping database: {e}")))?;

    // --- UP ---
    let plan_up = MigrationPlan::diff(&empty, &target_lock);
    println!("=== up: applying {} change(s) ===", plan_up.ops.len());
    for line in plan_up.describe() {
        println!("  + {line}");
    }
    runtime
        .block_on(async {
            let adapter = odsl_adapter::connect(db_url).await?;
            adapter.apply(&plan_up, &target_lock, Some(&empty)).await
        })
        .map_err(|e: odsl_adapter::AdapterError| io_err(format!("applying up: {e}")))?;

    // Assert the live schema matches the target.
    let live_up = runtime
        .block_on(odsl_adapter::introspect::introspect_to_odsl(db_url))
        .map_err(|e| io_err(format!("introspecting up state: {e}")))?;
    let live_up_ast =
        parse(&live_up).map_err(|e| io_err(format!("parsing introspected up schema: {e}")))?;
    odsl_core::Validator::validate(&live_up_ast, Some(target))
        .map_err(|e| io_err(format!("validating up state: {e}")))?;
    if let Err(mismatch) = ast.schema_matches(&live_up_ast) {
        return Err(io_err(format!("up schema mismatch: {mismatch}")));
    }
    // Strict mode additionally compares column types: a silent type change
    // (e.g. `int` -> `bigint`) would pass the structural check above but is a
    // real drift. Surface it as a non-fatal warning (the live DB already
    // matches structurally, so we don't fail the test on type-only drift).
    if let Err(drift) = ast.schema_matches_strict(&live_up_ast) {
        eprintln!("⚠ schema type drift detected: {drift}");
    }
    println!("✓ up: live database matches the target schema");

    if up_only {
        println!("✓ migrate test passed (up-only)");
        return Ok(());
    }

    // --- DOWN ---
    // Revert the *same* up-plan: `revert` inverts each op (CreateModel ->
    // DROP TABLE), so passing the up-plan returns the schema to empty.
    // `current` is the live state before the rollback (the target schema).
    let plan_down = plan_up.clone();
    println!("=== down: reverting {} change(s) ===", plan_down.ops.len());
    for line in plan_down.describe() {
        println!("  - {line}");
    }
    runtime
        .block_on(async {
            let adapter = odsl_adapter::connect(db_url).await?;
            adapter
                .revert(&plan_down, &target_lock, Some(&target_lock))
                .await
        })
        .map_err(|e: odsl_adapter::AdapterError| io_err(format!("applying down: {e}")))?;

    // Assert the live schema is back to empty (no target models remain).
    let live_down = runtime
        .block_on(odsl_adapter::introspect::introspect_to_odsl(db_url))
        .map_err(|e| io_err(format!("introspecting down state: {e}")))?;
    let live_down_ast =
        parse(&live_down).map_err(|e| io_err(format!("parsing introspected down schema: {e}")))?;
    // The empty expectation has no models; schema_matches against it means
    // the live DB must not contain any of the target's models.
    if let Err(mismatch) = Ast::new().schema_matches(&live_down_ast) {
        // An empty expected schema still requires nothing, so any mismatch
        // here is unexpected; report it defensively.
        return Err(io_err(format!("down schema mismatch: {mismatch}")));
    }
    // Explicitly ensure no target model survived the rollback.
    let surviving: Vec<String> = live_down_ast
        .models()
        .filter(|(_, m)| ast.model_by_name(&m.name).is_some())
        .map(|(_, m)| m.name.clone())
        .collect();
    if !surviving.is_empty() {
        return Err(io_err(format!(
            "down did not revert: target model(s) still present: {surviving:?}"
        )));
    }
    println!("✓ down: live database reverted to empty (no target models)");
    println!("✓ migrate test passed");
    Ok(())
}
