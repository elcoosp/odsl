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
    },
    /// Diff the schema against `osdl.lock` and manage migrations.
    Migrate {
        #[command(subcommand)]
        action: MigrateAction,
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
    },
}

fn main() -> Result<(), OsdlError> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Command::Init { path } => cmd_init(&path),
        Command::Build { input, target, out } => cmd_build(&input, target, &out),
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
            } => cmd_migrate_up(&input, target, db_url),
        },
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

fn cmd_build(
    input: &std::path::Path,
    target: Target,
    out: &std::path::Path,
) -> Result<(), OsdlError> {
    let ast = load_ast(input, target)?;
    let files = match target {
        Target::SeaOrmSqlite | Target::SeaOrmPostgres => {
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

    // Apply against a live database when a connection URL is provided.
    if let Some(url) = &db_url {
        let runtime =
            tokio::runtime::Runtime::new().map_err(|e| io_err(format!("tokio runtime: {e}")))?;
        let target_lock = Lockfile::from_ast(&ast);
        let applied = runtime
            .block_on(osdl_adapter::connect(url))
            .map_err(|e| io_err(format!("connecting to {url}: {e}")))?;
        let statements = runtime
            .block_on(applied.apply(&plan, &target_lock, Some(&current)))
            .map_err(|e| io_err(format!("applying migration: {e}")))?;
        for stmt in &statements {
            println!("  applied: {stmt}");
        }
        println!("applied {} change(s) to {url}", statements.len());
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
