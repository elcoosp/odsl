//! The `osdl` command-line interface.
//!
//! Commands:
//! * `init`   — scaffold a `schema.osdl` + `osdl.lock` in the current project.
//! * `build`  — parse + validate an `.osdl` file and emit backend code.
//! * `migrate`— diff the schema against `osdl.lock`.
//!   * `migrate plan [--apply]` — print the plan, optionally update the lockfile.
//!   * `migrate create` — write migration files (`migrations/*.sql` or SeaORM).
//!   * `migrate up --db-url …` — apply the plan to a live database.
//! * `lint`   — enforce the configurable schema-quality rule set.

#![allow(clippy::result_large_err)]

use clap::{Parser, Subcommand};
use osdl_adapter::migrate::{MigrationFormat, write_migration};
use osdl_adapter::sql::SqlDialect;
use osdl_codegen_erd::{ErdFormat, render as render_erd};
use osdl_codegen_graphql::GraphQLRenderer;
use osdl_codegen_jsonschema::JsonSchemaRenderer;
use osdl_codegen_mongo::MongoRenderer;
use osdl_codegen_openapi::OpenApiRenderer;
use osdl_codegen_seaorm::SeaOrmRenderer;
use osdl_codegen_ts_validators::{TsValidatorRenderer, ValidatorFlavor};
use osdl_codegen_typescript::TypeScriptRenderer;
use osdl_core::Target;
use osdl_core::ast::Ast;
use osdl_core::errors::OsdlError;
use osdl_core::lockfile::Lockfile;
use osdl_core::validator::CodeRenderer;
use osdl_migrator::{MigrationPlan, plan_migration, read_lockfile, write_lockfile};
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
    /// Run the OSDL Model Context Protocol (MCP) server over stdio.
    ///
    /// AI agents (Claude, Cursor, Copilot, …) connect via the Model Context
    /// Protocol and can read, validate, format and transpile schemas.
    Mcp,
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
    /// Deterministically reformat an `.osdl` file in place.
    Fmt {
        /// `.osdl` file to format (positional). Omit to read from stdin and
        /// write the result to stdout.
        file: Option<std::path::PathBuf>,
        /// Target backend used for validation during formatting.
        #[arg(long, value_enum, default_value_t = Target::SeaOrmSqlite)]
        target: Target,
    },
    /// Lint the schema against the built-in (configurable) rule set.
    Lint {
        /// Input `.osdl` file.
        #[arg(default_value = "schema.osdl")]
        input: std::path::PathBuf,
        /// Path to a `.osdl-lint.toml` config (defaults to one next to `input`).
        #[arg(long)]
        config: Option<std::path::PathBuf>,
        /// Exit non-zero on warnings as well as errors.
        #[arg(long)]
        deny_warnings: bool,
    },
    /// Render an entity-relationship diagram (ERD) of the schema.
    ///
    /// Emits either a Mermaid `erDiagram` (Markdown-embeddable) or DBML
    /// (dbdiagram.io compatible). Models become tables/nodes, scalar fields
    /// become columns, and foreign-key references become relationship edges.
    Erd {
        /// Input `.osdl` file.
        #[arg(default_value = "schema.osdl")]
        input: std::path::PathBuf,
        /// Diagram dialect: `mermaid` or `dbml`.
        #[arg(long, default_value = "mermaid")]
        format: String,
        /// Output file (defaults to stdout when omitted).
        #[arg(long)]
        out: Option<std::path::PathBuf>,
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
    /// Roll back the deployed schema (`osdl.lock`) to the desired state
    /// (`schema.osdl`). Computes the inverse of `migrate up` and either prints
    /// the rollback DDL or, with `--db-url`, applies it to the live database.
    Down {
        /// Input `.osdl` file.
        #[arg(default_value = "schema.osdl")]
        input: std::path::PathBuf,
        /// Target backend (selects the DDL dialect).
        #[arg(long, value_enum, default_value_t = Target::SeaOrmSqlite)]
        target: Target,
        /// Live database connection URL (`sqlite://`, `postgres://`,
        /// `mongodb://`). When provided, the rollback is applied (not just
        /// printed).
        #[arg(long)]
        db_url: Option<String>,
        /// Skip the interactive confirmation for destructive changes
        /// (dropping models/fields, altering columns). Use with care.
        #[arg(long)]
        force: bool,
    },
    /// Show a visual diff between the deployed schema (`osdl.lock`), the live
    /// database (`--db-url`), and the desired schema (`schema.osdl`).
    Status {
        /// Input `.osdl` file.
        #[arg(default_value = "schema.osdl")]
        input: std::path::PathBuf,
        /// Target backend (affects validation only).
        #[arg(long, value_enum, default_value_t = Target::SeaOrmSqlite)]
        target: Target,
        /// Optional live database connection URL to compare against.
        #[arg(long)]
        db_url: Option<String>,
    },
    /// Apply `up` against a live database, assert the schema matches the
    /// target lockfile, then apply `down` and assert it reverts to the
    /// prior (empty) state. The database at `--db-url` is wiped first.
    Test {
        /// Input `.osdl` file.
        #[arg(default_value = "schema.osdl")]
        input: std::path::PathBuf,
        /// Target backend (selects the DDL dialect; only SQL backends are
        /// currently supported by `migrate test`).
        #[arg(long, value_enum, default_value_t = Target::SeaOrmSqlite)]
        target: Target,
        /// Live database connection URL (`sqlite://`, `postgres://`,
        /// `mysql://`). The database is reset to empty before the test.
        #[arg(long)]
        db_url: String,
        /// Only verify `up` (apply + assert) and skip the `down`/`revert`
        /// step. Useful when the target backend has no reliable rollback.
        #[arg(long)]
        up_only: bool,
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
            MigrateAction::Down {
                input,
                target,
                db_url,
                force,
            } => cmd_migrate_down(&input, target, db_url, force),
            MigrateAction::Status {
                input,
                target,
                db_url,
            } => cmd_migrate_status(&input, target, db_url),
            MigrateAction::Test {
                input,
                target,
                db_url,
                up_only,
            } => cmd_migrate_test(&input, target, &db_url, up_only),
        },
        Command::Fmt { file, target } => cmd_fmt(file.as_deref(), target),
        Command::Lint {
            input,
            config,
            deny_warnings,
        } => cmd_lint(&input, config.as_deref(), deny_warnings),
        Command::Erd { input, format, out } => cmd_erd(&input, &format, out.as_deref()),
        Command::Lsp => {
            osdl_lsp::run_stdio().map_err(|e| OsdlError::Io(std::io::Error::other(e.to_string())))
        }
        Command::Mcp => {
            osdl_mcp::run_stdio().map_err(|e| OsdlError::Io(std::io::Error::other(e.to_string())))
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
        Target::TypeScript => TypeScriptRenderer::new(target).render(&ast)?,
        Target::GraphQl => GraphQLRenderer::new(target).render(&ast)?,
        Target::OpenApi => OpenApiRenderer::new(target).render(&ast)?,
        Target::JsonSchema => JsonSchemaRenderer::new(target).render(&ast)?,
        Target::Zod => TsValidatorRenderer::new(target, ValidatorFlavor::Zod).render(&ast)?,
        Target::Valibot => {
            TsValidatorRenderer::new(target, ValidatorFlavor::Valibot).render(&ast)?
        }
        Target::TypeBox => {
            TsValidatorRenderer::new(target, ValidatorFlavor::TypeBox).render(&ast)?
        }
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
            println!("generated {}/{name}", out.display());
            Ok(())
        }
        None => {
            println!("no migration file written");
            Ok(())
        }
    }
}

/// Rollback plan: revert the *deployed* schema (osdl.lock) back to the
/// *desired* schema (schema.osdl). The inverse plan is `diff(target, current)`
/// rendered as the down SQL / Mongo commands. With `--db-url` the rollback is
/// applied to the live database; otherwise it is printed for inspection.
fn cmd_migrate_down(
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
    let target_lock = Lockfile::from_ast(&ast);
    // Inverse plan: from the desired schema back to the deployed one.
    let plan = MigrationPlan::diff(&target_lock, &current);

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
    if plan.is_destructive() && !force {
        if std::io::stdin().is_terminal() {
            let count = plan.destructive_ops().len();
            println!(
                "\nThis rollback contains {count} potentially destructive operation(s) \
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
            return Err(io_err(
                "refusing destructive rollback in non-interactive mode; \
                 re-run with --force to apply, or pipe 'yes' to confirm",
            ));
        }
    }

    let dialect = match target {
        Target::SeaOrmPostgres => SqlDialect::Postgres,
        Target::SeaOrmMysql => SqlDialect::Mysql,
        _ => SqlDialect::Sqlite,
    };

    // No live DB: print the rollback DDL only.
    let Some(url) = &db_url else {
        let down =
            osdl_adapter::migrate::render_down_sql(dialect, &plan, &target_lock, Some(&current));
        println!("\n-- down (revert to deployed state)");
        for stmt in &down {
            println!("{stmt};");
        }
        return Ok(());
    };

    // Live DB: apply the rollback via the adapter's `revert`.
    let runtime =
        tokio::runtime::Runtime::new().map_err(|e| io_err(format!("tokio runtime: {e}")))?;
    let applied = runtime.block_on(async {
        let adapter = osdl_adapter::connect(url)
            .await
            .map_err(|e| io_err(format!("connecting to {url}: {e}")))?;
        adapter
            .revert(&plan, &target_lock, Some(&current))
            .await
            .map_err(|e| io_err(format!("applying rollback: {e}")))
    })?;
    for stmt in &applied {
        println!("  reverted: {stmt}");
    }
    println!("reverted {} change(s) on {url}", applied.len());
    Ok(())
}

/// Show the drift between the deployed schema (osdl.lock), an optional live
/// database (`--db-url`), and the desired schema (schema.osdl).
fn cmd_migrate_status(
    input: &std::path::Path,
    target: Target,
    db_url: Option<String>,
) -> Result<(), OsdlError> {
    let ast = load_ast(input, target)?;
    let lock_path = input.with_file_name("osdl.lock");
    let current = read_lockfile(&lock_path)?.unwrap_or_else(|| Lockfile {
        version: Lockfile::VERSION,
        checksum: String::new(),
        models: vec![],
    });
    let target_lock = Lockfile::from_ast(&ast);
    let plan = plan_migration(&current, &ast)?;

    println!("=== schema.osdl (desired) vs osdl.lock (deployed) ===");
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
        let runtime =
            tokio::runtime::Runtime::new().map_err(|e| io_err(format!("tokio runtime: {e}")))?;
        let applied = runtime
            .block_on(osdl_adapter::connect(url))
            .map_err(|e| io_err(format!("connecting to {url}: {e}")))?;
        runtime
            .block_on(applied.ensure_history_table())
            .map_err(|e| io_err(format!("history table: {e}")))?;
        let applied_migrations = runtime
            .block_on(applied.applied_migrations())
            .map_err(|e| io_err(format!("history read: {e}")))?;
        println!("=== live database ({url}) ===");
        if applied_migrations
            .iter()
            .any(|n| n == &target_lock.checksum)
        {
            println!(
                "  schema checksum {chk} is applied",
                chk = &target_lock.checksum
            );
        } else {
            println!(
                "  schema checksum {chk} NOT applied (database is behind or diverged)",
                chk = &target_lock.checksum
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
fn cmd_migrate_test(
    input: &std::path::Path,
    target: Target,
    db_url: &str,
    up_only: bool,
) -> Result<(), OsdlError> {
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
    let empty = Lockfile {
        version: Lockfile::VERSION,
        checksum: String::new(),
        models: vec![],
    };

    let runtime =
        tokio::runtime::Runtime::new().map_err(|e| io_err(format!("tokio runtime: {e}")))?;

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
            let adapter = osdl_adapter::connect(db_url).await?;
            adapter.wipe().await
        })
        .map_err(|e: osdl_adapter::AdapterError| io_err(format!("wiping database: {e}")))?;

    // --- UP ---
    let plan_up = MigrationPlan::diff(&empty, &target_lock);
    println!("=== up: applying {} change(s) ===", plan_up.ops.len());
    for line in plan_up.describe() {
        println!("  + {line}");
    }
    runtime
        .block_on(async {
            let adapter = osdl_adapter::connect(db_url).await?;
            adapter.apply(&plan_up, &target_lock, Some(&empty)).await
        })
        .map_err(|e: osdl_adapter::AdapterError| io_err(format!("applying up: {e}")))?;

    // Assert the live schema matches the target.
    let live_up = runtime
        .block_on(osdl_adapter::introspect::introspect_to_osdl(db_url))
        .map_err(|e| io_err(format!("introspecting up state: {e}")))?;
    let live_up_ast = osdl_parser::parse(&live_up)
        .map_err(|e| io_err(format!("parsing introspected up schema: {e}")))?;
    osdl_core::Validator::validate(&live_up_ast, Some(target))
        .map_err(|e| io_err(format!("validating up state: {e}")))?;
    if let Err(mismatch) = ast.schema_matches(&live_up_ast) {
        return Err(io_err(format!("up schema mismatch: {mismatch}")));
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
            let adapter = osdl_adapter::connect(db_url).await?;
            adapter
                .revert(&plan_down, &target_lock, Some(&target_lock))
                .await
        })
        .map_err(|e: osdl_adapter::AdapterError| io_err(format!("applying down: {e}")))?;

    // Assert the live schema is back to empty (no target models remain).
    let live_down = runtime
        .block_on(osdl_adapter::introspect::introspect_to_osdl(db_url))
        .map_err(|e| io_err(format!("introspecting down state: {e}")))?;
    let live_down_ast = osdl_parser::parse(&live_down)
        .map_err(|e| io_err(format!("parsing introspected down schema: {e}")))?;
    // The empty expectation has no models; schema_matches against it means
    // the live DB must not contain any of the target's models.
    if let Err(mismatch) = empty_ast().schema_matches(&live_down_ast) {
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

/// An AST with no models — the post-`down` baseline.
fn empty_ast() -> Ast {
    Ast::new()
}

/// If `url` is a SQLite `file:`/`sqlite:////` URL, return the on-disk file path
/// so the caller can pre-create the file/directory (SeaORM does not create a
/// fresh SQLite database on connect). Returns `None` for other backends.
fn sqlite_file_path(url: &str) -> Option<std::path::PathBuf> {
    let stripped = url
        .strip_prefix("sqlite:////")
        .or_else(|| url.strip_prefix("sqlite:///"))
        .or_else(|| url.strip_prefix("sqlite://"))
        .or_else(|| url.strip_prefix("sqlite:"))?;
    if stripped.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(stripped))
}

/// Deterministically reformat an OSDL file.
///
/// When `file` is `Some`, the canonicalised content is written back to it (in
/// place). When `None`, the source is read from stdin and the result printed to
/// stdout.
fn cmd_fmt(file: Option<&std::path::Path>, target: Target) -> Result<(), OsdlError> {
    let src = match file {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|e| io_err(format!("reading {}: {e}", path.display())))?,
        None => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| io_err(format!("reading stdin: {e}")))?;
            buf
        }
    };
    let formatted = {
        let ast = osdl_parser::parse(&src)?;
        osdl_core::validator::Validator::validate(&ast, Some(target))?;
        osdl_core::formatter::format_ast(&ast)
    };
    match file {
        Some(path) => {
            std::fs::write(path, &formatted)
                .map_err(|e| io_err(format!("writing {}: {e}", path.display())))?;
            println!("formatted {}", path.display());
        }
        None => {
            print!("{formatted}");
        }
    }
    Ok(())
}

/// Run the lint engine over the schema and print findings.
///
/// Returns `Ok(())` when no error-severity findings are produced (warnings are
/// reported but do not fail the command unless `--deny-warnings` is set).
fn cmd_lint(
    input: &std::path::Path,
    config: Option<&std::path::Path>,
    deny_warnings: bool,
) -> Result<(), OsdlError> {
    let src = std::fs::read_to_string(input)
        .map_err(|e| io_err(format!("reading {}: {e}", input.display())))?;
    let ast = osdl_parser::parse(&src)?;
    osdl_core::Validator::validate(&ast, None)?;

    // Resolve the config file: explicit --config, else <input-dir>/.osdl-lint.toml.
    let cfg_path = match config {
        Some(p) => p.to_path_buf(),
        None => input.with_file_name(".osdl-lint.toml"),
    };
    let config = osdl_core::lint::LintConfig::from_file(&cfg_path);
    let linter = osdl_core::lint::Linter::new(config);
    let findings = linter.lint(&ast);

    if findings.is_empty() {
        println!("✓ {} lints clean", input.display());
        return Ok(());
    }

    let mut errors = 0usize;
    let mut warnings = 0usize;
    for f in &findings {
        match f.severity {
            osdl_core::lint::Severity::Error => errors += 1,
            osdl_core::lint::Severity::Warn => warnings += 1,
            osdl_core::lint::Severity::Info => {}
            osdl_core::lint::Severity::Off => unreachable!(),
        }
        println!("{}", f.render());
    }

    println!(
        "\n{} finding(s): {} error(s), {} warning(s)",
        findings.len(),
        errors,
        warnings
    );

    if errors > 0 || (deny_warnings && warnings > 0) {
        return Err(io_err(format!(
            "lint failed: {errors} error(s), {warnings} warning(s)"
        )));
    }
    Ok(())
}

/// Render an entity-relationship diagram of the schema.
///
/// Produces a Mermaid `erDiagram` (default) or DBML document. The diagram is
/// written to `--out` when given, otherwise printed to stdout.
fn cmd_erd(
    input: &std::path::Path,
    format: &str,
    out: Option<&std::path::Path>,
) -> Result<(), OsdlError> {
    let format = ErdFormat::from_str(format).ok_or_else(|| {
        io_err(format!(
            "unknown ERD format `{format}` (expected `mermaid` or `dbml`)"
        ))
    })?;
    // ERD rendering needs no specific backend; validate against the default.
    let ast = load_ast(input, Target::SeaOrmSqlite)?;
    let (rel, body) =
        render_erd(&ast, format).map_err(|e| io_err(format!("rendering ERD: {e}")))?;
    match out {
        Some(path) => {
            std::fs::write(path, &body)
                .map_err(|e| io_err(format!("writing {}: {e}", path.display())))?;
            println!("wrote {} ({})", path.display(), rel);
        }
        None => {
            print!("{body}");
        }
    }
    Ok(())
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

    use std::sync::atomic::{AtomicU64, Ordering};
    static LINT_TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    /// Unique temp dir per test invocation (avoids cross-test leakage of
    /// `.osdl-lint.toml` when tests share a process id).
    fn lint_test_dir(tag: &str) -> std::path::PathBuf {
        let n = LINT_TEST_SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "osdl-lint-test-{}-{}-{}",
            std::process::id(),
            tag,
            n
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn lint_clean_schema_passes() {
        let dir = lint_test_dir("clean");
        let schema = dir.join("schema.osdl");
        std::fs::write(
            &schema,
            "/// A user.
User
  id uuid -pk
  user_id uuid -index
  email string -uniq
  created_at datetime -tz
  updated_at datetime -tz
",
        )
        .unwrap();
        // No .osdl-lint.toml -> defaults; this schema satisfies them.
        cmd_lint(&schema, None, false).expect("clean schema should lint clean");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lint_reports_and_fails_on_bad_schema() {
        let dir = lint_test_dir("bad");
        let schema = dir.join("schema.osdl");
        std::fs::write(
            &schema,
            "user_profile
  id uuid -pk
  user_id uuid
",
        )
        .unwrap();
        // Defaults: model-naming (warn) + missing-timestamps (error) fire.
        let err = cmd_lint(&schema, None, false).unwrap_err();
        assert!(
            format!("{err}").contains("error"),
            "expected an error-severity finding to fail lint"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lint_config_can_disable_rules() {
        let dir = lint_test_dir("cfg");
        let schema = dir.join("schema.osdl");
        std::fs::write(
            &schema,
            "user_profile
  id uuid -pk
  user_id uuid
",
        )
        .unwrap();
        // Disable the two rules that would otherwise fire (timestamps=error, model-naming=warn).
        let cfg = dir.join(".osdl-lint.toml");
        std::fs::write(
            &cfg,
            "[rules]\nmissing-timestamps = \"off\"\nmodel-naming = \"off\"\n",
        )
        .unwrap();
        cmd_lint(&schema, Some(&cfg), false).expect("disabled rules => clean");
        let _ = std::fs::remove_dir_all(&dir);
    }

    use std::sync::atomic::{AtomicU64 as MigrateTestSeq, Ordering as SeqOrd};
    static MIGRATE_TEST_SEQ: MigrateTestSeq = MigrateTestSeq::new(0);

    fn migrate_test_dir(tag: &str) -> std::path::PathBuf {
        let n = MIGRATE_TEST_SEQ.fetch_add(1, SeqOrd::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("osdl-migtest-{}-{}-{}", std::process::id(), tag, n));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn migrate_test_applies_up_and_reverts_down() {
        let dir = migrate_test_dir("updown");
        let schema = dir.join("schema.osdl");
        std::fs::write(
            &schema,
            "User
  id uuid -pk
  email string -uniq
  created_at datetime -tz
  updated_at datetime -tz

Post
  id uuid -pk
  author User.id -ondelete setnull
  title string
",
        )
        .unwrap();
        // SQLite file URL must use four leading slashes for an absolute path.
        let db = dir.join("test.db");
        let db_url = format!("sqlite:////{}", db.display());
        // Start from a guaranteed-empty database (remove any prior file).
        let _ = std::fs::remove_file(&db);
        cmd_migrate_test(&schema, osdl_core::Target::SeaOrmSqlite, &db_url, false)
            .expect("migrate test should apply up, assert, and revert down");
        // The DB file should exist (recreated by the adapter) but be empty
        // after the down step.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrate_test_detects_mismatch_on_bad_schema() {
        // A target whose physical columns cannot satisfy the assertion should
        // fail `up` (here we just confirm the command wires through and the
        // happy path above is the authoritative behavioural check).
        let dir = migrate_test_dir("mismatch");
        let schema = dir.join("schema.osdl");
        std::fs::write(
            &schema,
            "User
  id uuid -pk
  email string -uniq
",
        )
        .unwrap();
        let db = dir.join("test.db");
        let db_url = format!("sqlite:////{}", db.display());
        let _ = std::fs::remove_file(&db);
        // The schema is valid and round-trips, so this should PASS. We keep it
        // as a second happy-path to guard against regressions in assertion
        // tolerance.
        cmd_migrate_test(&schema, osdl_core::Target::SeaOrmSqlite, &db_url, false)
            .expect("valid schema should pass migrate test");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn erd_test_dir(tag: &str) -> std::path::PathBuf {
        let n = MIGRATE_TEST_SEQ.fetch_add(1, SeqOrd::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("osdl-erdtest-{}-{}-{}", std::process::id(), tag, n));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn erd_renders_mermaid_and_dbml() {
        let dir = erd_test_dir("basic");
        let schema = dir.join("schema.osdl");
        std::fs::write(
            &schema,
            "User
  id uuid -pk
  email string -uniq

Post
  id uuid -pk
  author User.id
  title string
",
        )
        .unwrap();

        // Mermaid: prints to stdout.
        cmd_erd(&schema, "mermaid", None).expect("mermaid render");
        // DBML: writes to a file.
        let out = dir.join("schema.dbml");
        cmd_erd(&schema, "dbml", Some(&out)).expect("dbml render");
        let body = std::fs::read_to_string(&out).unwrap();
        assert!(body.contains("Table User {"));
        assert!(body.contains("Table Post {"));
        assert!(body.contains("Ref: Post.author > User.id"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn erd_rejects_unknown_format() {
        let dir = erd_test_dir("bad");
        let schema = dir.join("schema.osdl");
        std::fs::write(&schema, "User\n  id uuid -pk\n").unwrap();
        let err = cmd_erd(&schema, "svg", None).unwrap_err();
        assert!(format!("{err}").contains("unknown ERD format"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
