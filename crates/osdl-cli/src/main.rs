//! The `osdl` command-line interface.
//!
//! Commands:
//! * `init`   — scaffold a `schema.osdl` + `osdl.lock` in the current project.
//! * `build`  — parse + validate an `.osdl` file and emit backend code.
//! * `migrate`— diff the schema against `osdl.lock`, print the plan, and update
//!   the lockfile.

#![allow(clippy::result_large_err)]

use clap::{Parser, Subcommand};
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
    /// Show and apply migrations against `osdl.lock`.
    Migrate {
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
}

fn main() -> Result<(), OsdlError> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Command::Init { path } => cmd_init(&path),
        Command::Build { input, target, out } => cmd_build(&input, target, &out),
        Command::Migrate {
            input,
            target,
            apply,
        } => cmd_migrate(&input, target, apply),
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
        .map_err(|e| OsdlError::Io(format!("reading {}: {e}", input.display())))?;
    let ast = parse(&src)?;
    osdl_core::Validator::validate(&ast, Some(target))?;
    Ok(ast)
}

fn cmd_init(path: &std::path::Path) -> Result<(), OsdlError> {
    if path.exists() {
        return Err(OsdlError::Io(format!("{} already exists", path.display())));
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

fn cmd_migrate(input: &std::path::Path, target: Target, apply: bool) -> Result<(), OsdlError> {
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
