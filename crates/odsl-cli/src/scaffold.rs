//! Project scaffolding: `odsl init` and `odsl pull`.

use crate::project::{io_err, new_runtime};
use odsl_core::Target;
use odsl_core::errors::OdslError;
use odsl_core::lockfile::Lockfile;
use odsl_migrator::write_lockfile;
use odsl_parser::parse;

pub fn cmd_init(path: &std::path::Path) -> Result<(), OdslError> {
    if path.exists() {
        return Err(io_err(format!("{} already exists", path.display())));
    }
    let skeleton = "# ODSL schema\n# Run `odsl build` to generate backend code.\n\nUser\n  id uuid -pk\n  email string -uniq\n  created_at datetime -tz\n";
    std::fs::write(path, skeleton)?;
    // Seed an empty lockfile so the first build has a stable baseline.
    let lock = path.with_file_name("odsl.lock");
    let ast = parse(skeleton)?;
    odsl_core::Validator::validate(&ast, Some(Target::SeaOrmSqlite))?;
    write_lockfile(&lock, &Lockfile::from_ast(&ast))?;
    tracing::info!(schema = %path.display(), lock = %lock.display(), "initialized project");
    println!("created {} and {}", path.display(), lock.display());
    Ok(())
}

/// Reverse-engineer a live database (via `--db-url`) into an ODSL schema file.
pub fn cmd_pull(db_url: &str, out: &std::path::Path) -> Result<(), OdslError> {
    use odsl_adapter::introspect::introspect_to_odsl;
    let rt = new_runtime()?;
    let odsl = rt
        .block_on(introspect_to_odsl(db_url))
        .map_err(|e| io_err(format!("introspecting {db_url}: {e}")))?;
    std::fs::write(out, &odsl).map_err(|e| io_err(format!("writing {}: {e}", out.display())))?;
    println!("wrote {}", out.display());
    Ok(())
}
