//! Shared project-loading and small utilities used by the CLI commands.
//!
//! Every command starts from the same primitives: parse + validate the
//! desired schema, resolve the `odsl.lock` next to it and read the currently
//! deployed state. Centralizing them here keeps the command modules focused
//! on their own behaviour.

use odsl_core::Target;
use odsl_core::ast::Ast;
use odsl_core::errors::OdslError;
use odsl_core::lockfile::Lockfile;
use odsl_migrator::read_lockfile;
use odsl_parser::parse_project;

/// Build an [`OdslError::Io`] from an ad-hoc message (e.g. a guard condition
/// that is not a real `std::io::Error`). Real IO results propagate via `?`.
pub fn io_err(msg: impl Into<String>) -> OdslError {
    OdslError::Io(std::io::Error::other(msg.into()))
}

/// Parse and validate the schema at `input`, following `use` declarations and
/// merging every imported module (roadmap Phase 1.7 — multi-file modules).
pub fn load_ast(input: &std::path::Path, target: Target) -> Result<Ast, OdslError> {
    let project = parse_project(input)?;
    odsl_core::Validator::validate(&project.ast, Some(target))?;
    Ok(project.ast)
}

/// Everything the migrate / doctor commands need to know about one project.
pub struct ProjectState {
    /// The desired schema (parsed + validated against `target`).
    pub ast: Ast,
    /// The `odsl.lock` file next to the input schema.
    pub lock_path: std::path::PathBuf,
    /// The currently deployed lockfile (empty when none exists yet).
    pub current: Lockfile,
}

/// Load a full [`ProjectState`] in one step — the shared prologue of the
/// `migrate` subcommands.
pub fn load_project(input: &std::path::Path, target: Target) -> Result<ProjectState, OdslError> {
    let ast = load_ast(input, target)?;
    let lock_path = input.with_file_name("odsl.lock");
    let current = read_lockfile(&lock_path)?.unwrap_or_else(Lockfile::empty);
    Ok(ProjectState {
        ast,
        lock_path,
        current,
    })
}

/// A fresh tokio runtime. The CLI drives async adapters synchronously, one
/// runtime per command invocation.
pub fn new_runtime() -> Result<tokio::runtime::Runtime, OdslError> {
    tokio::runtime::Runtime::new().map_err(|e| io_err(format!("tokio runtime: {e}")))
}

/// Connect to `url`, ensure the migration-history table exists and return the
/// adapter plus the recorded schema checksums. Shared by `migrate up`,
/// `migrate status` and `doctor`.
pub fn history_entries(
    runtime: &tokio::runtime::Runtime,
    url: &str,
) -> Result<(Box<dyn odsl_adapter::SchemaAdapter>, Vec<String>), OdslError> {
    let adapter = runtime
        .block_on(odsl_adapter::connect(url))
        .map_err(|e| io_err(format!("connecting to {url}: {e}")))?;
    runtime
        .block_on(adapter.ensure_history_table())
        .map_err(|e| io_err(format!("history table: {e}")))?;
    let applied = runtime
        .block_on(adapter.applied_migrations())
        .map_err(|e| io_err(format!("history read: {e}")))?;
    Ok((adapter, applied))
}

/// Local-time HH:MM:SS for watch-mode log lines.
pub fn now_str() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// If `url` is a SQLite `file:`/`sqlite:////` URL, return the on-disk file path
/// so the caller can pre-create the file/directory (SeaORM does not create a
/// fresh SQLite database on connect). Returns `None` for other backends.
pub fn sqlite_file_path(url: &str) -> Option<std::path::PathBuf> {
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
