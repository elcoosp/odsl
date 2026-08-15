//! The `osdl` command-line interface.
//!
//! Commands:
//! * `init`   — scaffold a `schema.osdl` + `osdl.lock` in the current project.
//! * `build`  — parse + validate an `.osdl` file and emit backend code.
//! * `migrate`— diff the schema against `osdl.lock`.
//!   * `migrate plan [--apply]` — print the plan, optionally update the lockfile.
//!   * `migrate create` — write migration files (`migrations/*.sql` or SeaORM).
//!   * `migrate up --db-url …` — apply the plan to a live database.

#![allow(clippy::result_large_err)]

use clap::{Parser, Subcommand};
use osdl_adapter::migrate::{MigrationFormat, write_migration};
use osdl_adapter::sql::SqlDialect;
use osdl_codegen_mongo::MongoRenderer;
use osdl_codegen_seaorm::SeaOrmRenderer;
use osdl_core::Target;
use osdl_core::ast::Ast;
use osdl_core::errors::OsdlError;
use osdl_core::lockfile::Lockfile;
use osdl_core::validator::CodeRenderer;
use osdl_migrator::{plan_migration, read_lockfile, write_lockfile};
use osdl_parser::parse;
use std::io::IsTerminal;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "osdl",
    version,
    about = "OSDL schema compiler & migration tool"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Increase log verbosity (-v, -vv).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold a new `schema.osdl` + `osdl.lock`.
    Init {
        /// Path to the schema file to create.
        #[arg(default_value = "schema.osdl")]
        path: std::path::PathBuf,
    },
    /// Parse, validate and emit backend code.
    Build {
        /// Input `.osdl` file.
        #[arg(default_value = "schema.osdl")]
        input: std::path::PathBuf,
        /// Target backend.
        #[arg(long, value_enum, default_value_t = Target::SeaOrmSqlite)]
        target: Target,
        /// Output directory for generated code.
        #[arg(long, default_value = "src/generated")]
        out: std::path::PathBuf,
        /// Watch `input` and rebuild on change (until interrupted).
        #[arg(long)]
        watch: bool,
    },
    /// Diff the schema against `osdl.lock` and manage migrations.
    Migrate {
        #[command(subcommand)]
        action: MigrateAction,
    },
    /// Run the OSDL language server (LSP) over stdio.
    ///
    /// Editors connect via the Language Server Protocol; diagnostics are
    /// published on open/change using the same parse+validate pipeline as
    /// `osdl build`.
    Lsp,
    /// Reverse-engineer a live database into an OSDL schema.
    ///
    /// Connects to `--db-url` (sqlite://, postgres://, mysql://), reads the
    /// catalog and writes the inferred schema to `schema.osdl` (or `--out`).
    Pull {
        /// Database connection URL (`sqlite://`, `postgres://`, `mysql://`).
        #[arg(long)]
        db_url: String,
        /// Output `.osdl` file (defaults to `schema.osdl`).
        #[arg(default_value = "schema.osdl")]
        out: std::path::PathBuf,
    },
}

/// Sub-actions of `osdl migrate`.
#[derive(Subcommand)]
enum MigrateAction {
    /// Print the migration plan (optionally write the lockfile).
    Plan {
        /// Input `.osdl` file.
        #[arg(default_value = "schema.osdl")]
        input: std::path::PathBuf,
        /// Target backend (affects validation only).
        #[arg(long, value_enum, default_value_t = Target::SeaOrmSqlite)]
        target: Target,
        /// Write the new lockfile after computing the plan.
        #[arg(long)]
        apply: bool,
    },
    /// Generate migration files from the schema diff (no DB connection needed).
    Create {
        /// Input `.osdl` file.
        #[arg(default_value = "schema.osdl")]
        input: std::path::PathBuf,
        /// Target backend (selects the DDL dialect).
        #[arg(long, value_enum, default_value_t = Target::SeaOrmSqlite)]
        target: Target,
        /// Output directory for migration files.
        #[arg(long, default_value = "migrations")]
        out: std::path::PathBuf,
        /// Emit SeaORM `up`/`down` Rust modules instead of `.sql`.
        #[arg(long)]
        sea_orm: bool,
    },
    /// Apply the plan to a live database at `--db-url` (implies writing the lockfile).
    Up {
        /// Input `.osdl` file.
        #[arg(default_value = "schema.osdl")]
        input: std::path::PathBuf,
        /// Target backend (affects validation only).
        #[arg(long, value_enum, default_value_t = Target::SeaOrmSqlite)]
        target: Target,
        /// Database connection URL (`sqlite://`, `postgres://`, `mongodb://`).
        #[arg(long)]
        db_url: Option<String>,
        /// Skip the interactive confirmation for destructive changes
        /// (dropping models/fields, altering columns). Use with care.
        #[arg(long)]
        force: bool,
    },
}

fn main() -> Result<(), OsdlError> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Command::Init { path } => cmd_init(&path),
        Command::Build {
            input,
            target,
            out,
            watch,
        } => {
            if watch {
                cmd_build_watch(&input, target, &out)
            } else {
                run_build(&input, target, &out)
            }
        }
        Command::Migrate { action } => match action {
            MigrateAction::Plan {
                input,
                target,
                apply,
            } => cmd_migrate_plan(&input, target, apply),
            MigrateAction::Create {
                input,
                target,
                out,
                sea_orm,
            } => cmd_migrate_create(&input, target, &out, sea_orm),
            MigrateAction::Up {
                input,
                target,
                db_url,
                force,
            } => cmd_migrate_up(&input, target, db_url, force),
        },
        Command::Lsp => {
            osdl_lsp::run_stdio().map_err(|e| OsdlError::Io(std::io::Error::other(e.to_string())))
        }
        Command::Pull { db_url, out } => cmd_pull(&db_url, &out),
    }
}

fn init_tracing(verbose: u8) {
    let default = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

fn load_ast(input: &std::path::Path, target: Target) -> Result<Ast, OsdlError> {
    let src = std::fs::read_to_string(input)
        .map_err(|e| io_err(format!("reading {}: {e}", input.display())))?;
    let ast = parse(&src)?;
    osdl_core::Validator::validate(&ast, Some(target))?;
    Ok(ast)
}

/// Build an [`OsdlError::Io`] from an ad-hoc message (e.g. a guard condition
/// that is not a real `std::io::Error`). Real IO results propagate via `?`.
fn io_err(msg: impl Into<String>) -> OsdlError {
    OsdlError::Io(std::io::Error::other(msg.into()))
}

fn cmd_init(path: &std::path::Path) -> Result<(), OsdlError> {
    if path.exists() {
        return Err(io_err(format!("{} already exists", path.display())));
    }
    let skeleton = "# OSDL schema\n# Run `osdl build` to generate backend code.\n\nUser\n  id uuid -pk\n  email string -uniq\n  created_at datetime -tz\n";
    std::fs::write(path, skeleton)?;
    // Seed an empty lockfile so the first build has a stable baseline.
    let lock = path.with_file_name("osdl.lock");
    let ast = parse(skeleton)?;
    osdl_core::Validator::validate(&ast, Some(Target::SeaOrmSqlite))?;
    write_lockfile(&lock, &Lockfile::from_ast(&ast))?;
    tracing::info!(schema = %path.display(), lock = %lock.display(), "initialized project");
    println!("created {} and {}", path.display(), lock.display());
    Ok(())
}

/// Reverse-engineer a live database (via `--db-url`) into an OSDL schema file.
fn cmd_pull(db_url: &str, out: &std::path::Path) -> Result<(), OsdlError> {
    use osdl_adapter::introspect::introspect_to_osdl;
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| OsdlError::Io(std::io::Error::other(e.to_string())))?;
    let osdl = rt
        .block_on(introspect_to_osdl(db_url))
        .map_err(|e| OsdlError::Io(std::io::Error::other(e)))?;
    std::fs::write(out, &osdl).map_err(|e| {
        OsdlError::Io(std::io::Error::other(format!(
            "writing {}: {e}",
            out.display()
        )))
    })?;
    println!("wrote {}", out.display());
    Ok(())
}

/// Watch `input` for changes and rebuild on every modification until the
/// process is interrupted (Ctrl-C). Uses the `notify` crate's OS-native file
/// watcher; the first build runs immediately.
fn cmd_build_watch(
    input: &std::path::Path,
    target: Target,
    out: &std::path::Path,
) -> Result<(), OsdlError> {
    use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc;
    use std::time::Duration;

    let dir = input
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let watch_name = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "*.osdl".to_string());

    // Initial build.
    match run_build(input, target, out) {
        Ok(()) => println!("watching {} (Ctrl-C to stop)", input.display()),
        Err(e) => eprintln!("initial build failed: {e}"),
    }

    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            let _ = tx.send(res);
        },
        Config::default().with_poll_interval(Duration::from_millis(200)),
    )
    .map_err(|e| io_err(format!("creating watcher: {e}")))?;
    watcher
        .watch(&dir, RecursiveMode::NonRecursive)
        .map_err(|e| io_err(format!("watching {}: {e}", dir.display())))?;

    for res in rx {
        match res {
            Ok(event) => {
                // Only react to modifications of the watched file.
                if !event_triggers_rebuild(&event, &watch_name) {
                    continue;
                }
                match run_build(input, target, out) {
                    Ok(()) => println!("rebuilt at {}", now_str()),
                    Err(e) => eprintln!("build failed at {}: {e}", now_str()),
                }
            }
            Err(e) => eprintln!("watch error: {e}"),
        }
    }
    Ok(())
}

/// Whether a filesystem event should trigger a rebuild: a *modify* event on the
/// exact watched file (ignoring events for other paths or non-modify events).
fn event_triggers_rebuild(event: &notify::Event, watch_name: &str) -> bool {
    let is_modify = matches!(event.kind, notify::EventKind::Modify(_));
    let relevant = event.paths.iter().any(|p| {
        p.file_name()
            .map(|n| n.to_string_lossy() == watch_name)
            .unwrap_or(false)
    });
    is_modify && relevant
}

/// Build (parse + validate + codegen) once. Returns `Ok` on success.
fn run_build(
    input: &std::path::Path,
    target: Target,
    out: &std::path::Path,
) -> Result<(), OsdlError> {
    let ast = load_ast(input, target)?;
    let files = match target {
        Target::SeaOrmSqlite | Target::SeaOrmPostgres | Target::SeaOrmMysql => {
            SeaOrmRenderer::new(target).render(&ast)?
        }
        Target::Mongo => MongoRenderer::new(target).render(&ast)?,
    };
    std::fs::create_dir_all(out)?;
    for (rel, contents) in &files {
        let full = out.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&full, contents)?;
    }
    tracing::info!(target = ?target, files = files.len(), out = %out.display(), "generated code");
    println!("generated {} files into {}", files.len(), out.display());
    Ok(())
}

/// Local-time HH:MM:SS for watch-mode log lines.
fn now_str() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// One-line description of a single op (used by the destructive-guard prompt).
fn describe_op(op: &osdl_migrator::MigrationOp) -> String {
    match op {
        osdl_migrator::MigrationOp::DropModel { model } => format!("drop model {model}"),
        osdl_migrator::MigrationOp::DropField { model, field } => {
            format!("drop field {model}.{field}")
        }
        osdl_migrator::MigrationOp::AlterField { model, field, .. } => {
            format!("alter field {model}.{field}")
        }
        osdl_migrator::MigrationOp::CreateModel { .. }
        | osdl_migrator::MigrationOp::AddField { .. } => String::new(),
    }
}

fn cmd_migrate_plan(input: &std::path::Path, target: Target, apply: bool) -> Result<(), OsdlError> {
    let ast = load_ast(input, target)?;
    let lock_path = input.with_file_name("osdl.lock");
    let current = read_lockfile(&lock_path)?.unwrap_or_else(|| Lockfile {
        version: Lockfile::VERSION,
        checksum: String::new(),
        models: vec![],
    });
    let plan = plan_migration(&current, &ast)?;
    for line in plan.describe() {
        println!("  {line}");
    }
    if plan.ops.is_empty() {
        println!("no changes");
    }
    if apply {
        write_lockfile(&lock_path, &Lockfile::from_ast(&ast))?;
        tracing::info!(lock = %lock_path.display(), "lockfile updated");
        println!("updated {}", lock_path.display());
    }
    Ok(())
}

fn cmd_migrate_up(
    input: &std::path::Path,
    target: Target,
    db_url: Option<String>,
    force: bool,
) -> Result<(), OsdlError> {
    let ast = load_ast(input, target)?;
    let lock_path = input.with_file_name("osdl.lock");
    let current = read_lockfile(&lock_path)?.unwrap_or_else(|| Lockfile {
        version: Lockfile::VERSION,
        checksum: String::new(),
        models: vec![],
    });
    let plan = plan_migration(&current, &ast)?;
    for line in plan.describe() {
        println!("  {line}");
    }

    if plan.ops.is_empty() {
        println!("no changes");
    }

    // Guard destructive operations (drop model/field, alter column) unless
    // the user explicitly passes --force or confirms interactively.
    if plan.is_destructive() && !force {
        if std::io::stdin().is_terminal() {
            let count = plan.destructive_ops().len();
            println!(
                "\nThis plan contains {count} potentially destructive operation(s) \
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
                return Ok(());
            }
        } else {
            // Non-interactive (piped) without --force: refuse destructive ops.
            return Err(io_err(
                "refusing destructive migration in non-interactive mode; \
                 re-run with --force to apply, or pipe 'yes' to confirm",
            ));
        }
    }

    // Apply against a live database when a connection URL is provided.
    if let Some(url) = &db_url {
        let runtime =
            tokio::runtime::Runtime::new().map_err(|e| io_err(format!("tokio runtime: {e}")))?;
        let target_lock = Lockfile::from_ast(&ast);
        let applied = runtime
            .block_on(osdl_adapter::connect(url))
            .map_err(|e| io_err(format!("connecting to {url}: {e}")))?;
        // Idempotency: skip if this exact schema state was already applied.
        runtime
            .block_on(applied.ensure_history_table())
            .map_err(|e| io_err(format!("history table: {e}")))?;
        let schema_key = target_lock.checksum.clone();
        let already = runtime
            .block_on(applied.applied_migrations())
            .map_err(|e| io_err(format!("history read: {e}")))?;
        if already.iter().any(|n| n == &schema_key) {
            println!("schema {schema_key} already applied; skipping");
        } else {
            let statements = runtime
                .block_on(applied.apply(&plan, &target_lock, Some(&current)))
                .map_err(|e| io_err(format!("applying migration: {e}")))?;
            for stmt in &statements {
                println!("  applied: {stmt}");
            }
            println!("applied {} change(s) to {url}", statements.len());
            runtime
                .block_on(applied.record_applied(&schema_key, &schema_key))
                .map_err(|e| io_err(format!("recording migration: {e}")))?;
        }
    }

    // `up` always records the new baseline lockfile.
    write_lockfile(&lock_path, &Lockfile::from_ast(&ast))?;
    tracing::info!(lock = %lock_path.display(), "lockfile updated");
    println!("updated {}", lock_path.display());
    Ok(())
}

fn cmd_migrate_create(
    input: &std::path::Path,
    target: Target,
    out: &std::path::Path,
    sea_orm: bool,
) -> Result<(), OsdlError> {
    let ast = load_ast(input, target)?;
    let lock_path = input.with_file_name("osdl.lock");
    let current = read_lockfile(&lock_path)?.unwrap_or_else(|| Lockfile {
        version: Lockfile::VERSION,
        checksum: String::new(),
        models: vec![],
    });
    let plan = plan_migration(&current, &ast)?;
    for line in plan.describe() {
        println!("  {line}");
    }
    if plan.ops.is_empty() {
        println!("no changes; nothing to generate");
        return Ok(());
    }
    let dialect = match target {
        Target::SeaOrmPostgres => SqlDialect::Postgres,
        Target::SeaOrmMysql => SqlDialect::Mysql,
        _ => SqlDialect::Sqlite,
    };
    let format = if sea_orm {
        MigrationFormat::SeaOrm
    } else {
        MigrationFormat::Sql
    };
    let written = write_migration(
        out,
        format,
        dialect,
        &plan,
        &Lockfile::from_ast(&ast),
        Some(&current),
    )
    .map_err(|e| io_err(format!("writing migration: {e}")))?;
    match written {
        Some(name) => {
            println!("generated {}/{}", out.display(), name);
            Ok(())
        }
        None => {
            println!("no migration file written");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::{EventKind, event::ModifyKind};

    /// Helper to build a single-path notify event.
    fn ev(kind: EventKind, path: &str) -> notify::Event {
        notify::Event {
            kind,
            paths: vec![std::path::PathBuf::from(path)],
            attrs: Default::default(),
        }
    }

    #[test]
    fn watch_reacts_to_modify_of_watched_file() {
        let e = ev(EventKind::Modify(ModifyKind::Any), "/proj/schema.osdl");
        assert!(event_triggers_rebuild(&e, "schema.osdl"));
    }

    #[test]
    fn watch_ignores_modify_of_other_file() {
        let e = ev(EventKind::Modify(ModifyKind::Any), "/proj/other.osdl");
        assert!(!event_triggers_rebuild(&e, "schema.osdl"));
    }

    #[test]
    fn watch_ignores_non_modify_events() {
        let created = ev(
            EventKind::Create(notify::event::CreateKind::File),
            "/proj/schema.osdl",
        );
        let accessed = ev(
            EventKind::Access(notify::event::AccessKind::Any),
            "/proj/schema.osdl",
        );
        assert!(!event_triggers_rebuild(&created, "schema.osdl"));
        assert!(!event_triggers_rebuild(&accessed, "schema.osdl"));
    }
}
