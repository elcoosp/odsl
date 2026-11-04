//! Schema-quality commands: `odsl fmt`, `odsl lint`, `odsl erd` and
//! `odsl doctor`.

use crate::project::{history_entries, io_err, load_ast, new_runtime};
use odsl_codegen_erd::{ErdFormat, render as render_erd};
use odsl_core::Target;
use odsl_core::errors::OdslError;
use odsl_migrator::{plan_migration, read_lockfile};
use odsl_parser::parse;

/// Deterministically reformat an ODSL file.
///
/// When `file` is `Some`, the canonicalised content is written back to it (in
/// place). When `None`, the source is read from stdin and the result printed to
/// stdout.
pub fn cmd_fmt(file: Option<&std::path::Path>, target: Target) -> Result<(), OdslError> {
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
        let ast = parse(&src)?;
        odsl_core::validator::Validator::validate(&ast, Some(target))?;
        odsl_core::formatter::format_ast(&ast)
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
pub fn cmd_lint(
    input: &std::path::Path,
    config: Option<&std::path::Path>,
    deny_warnings: bool,
) -> Result<(), OdslError> {
    let src = std::fs::read_to_string(input)
        .map_err(|e| io_err(format!("reading {}: {e}", input.display())))?;
    let ast = parse(&src)?;
    odsl_core::Validator::validate(&ast, None)?;

    // Resolve the config file: explicit --config, else <input-dir>/.odsl-lint.toml.
    let cfg_path = match config {
        Some(p) => p.to_path_buf(),
        None => input.with_file_name(".odsl-lint.toml"),
    };
    let config = odsl_core::lint::LintConfig::from_file(&cfg_path);
    let linter = odsl_core::lint::Linter::new(config);
    let findings = linter.lint(&ast);

    if findings.is_empty() {
        println!("✓ {} lints clean", input.display());
        return Ok(());
    }

    let mut errors = 0usize;
    let mut warnings = 0usize;
    for f in &findings {
        match f.severity {
            odsl_core::lint::Severity::Error => errors += 1,
            odsl_core::lint::Severity::Warn => warnings += 1,
            odsl_core::lint::Severity::Info => {}
            odsl_core::lint::Severity::Off => unreachable!(),
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
pub fn cmd_erd(
    input: &std::path::Path,
    format: &str,
    out: Option<&std::path::Path>,
) -> Result<(), OdslError> {
    let format = ErdFormat::from_cli(format).ok_or_else(|| {
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

/// `odsl doctor` — a single health-check command (Phase 2.3).
///
/// Verifies the project and exits non-zero if any critical check fails: the
/// `schema.odsl` file must parse and validate (otherwise the project is
/// broken); the `odsl.lock` file must be present and readable; and there must be
/// no schema drift, i.e. the desired schema matches the deployed lockfile
/// (`plan_migration` produces no ops). With `--db-url`, the live database is
/// also checked for the current schema checksum. Lint findings are always
/// reported but never fail the doctor (they are advisory).
pub fn cmd_doctor(
    input: &std::path::Path,
    target: Target,
    db_url: Option<&str>,
) -> Result<(), OdslError> {
    let mut problems = 0usize;

    // 1. Parse + validate the desired schema.
    println!("=== schema.odsl ===");
    let ast = match load_ast(input, target) {
        Ok(a) => {
            println!(
                "  ✓ parsed and validated ({} models, {} views)",
                a.models().count(),
                a.views().count()
            );
            a
        }
        Err(e) => {
            println!("  ✗ parse/validate failed: {e}");
            return Err(e);
        }
    };

    // 2. Lockfile presence.
    println!("=== odsl.lock ===");
    let lock_path = input.with_file_name("odsl.lock");
    let current = match read_lockfile(&lock_path)? {
        Some(lf) => {
            println!("  ✓ lockfile present (checksum {})", lf.checksum);
            lf
        }
        None => {
            println!(
                "  ! no lockfile found at {} — run `odsl migrate plan --apply`",
                lock_path.display()
            );
            problems += 1;
            odsl_core::lockfile::Lockfile::empty()
        }
    };

    // 3. Drift: desired schema vs deployed lockfile.
    println!("=== drift ===");
    if current.models.is_empty() && current.views.is_empty() {
        println!("  ! no deployed schema to diff against (lockfile empty)");
        problems += 1;
    } else {
        let plan = plan_migration(&current, &ast)?;
        if plan.ops.is_empty() {
            println!("  ✓ in sync — no pending migrations");
        } else {
            println!("  ✗ drift detected — {} pending change(s):", plan.ops.len());
            for line in plan.describe() {
                println!("    + {line}");
            }
            problems += 1;
        }
    }

    // 4. Lint summary (advisory). Style rules (missing timestamps / model doc)
    //    are reported but never fail the doctor — they describe boilerplate the
    //    `config` block exists to remove, not a project-health problem.
    println!("=== lint ===");
    let linter = odsl_core::lint::Linter::new(odsl_core::lint::LintConfig::default());
    let findings = linter.lint(&ast);
    if findings.is_empty() {
        println!("  ✓ no lint findings");
    } else {
        let errors = findings
            .iter()
            .filter(|f| f.severity == odsl_core::lint::Severity::Error)
            .count();
        let warnings = findings.len() - errors;
        println!("  ! {errors} error(s), {warnings} warning(s)");
        for f in &findings {
            println!("    - [{}] {}: {}", f.severity, f.rule.as_str(), f.message);
        }
        // Only *non-style* errors count as project-health problems.
        let hard_errors = findings
            .iter()
            .filter(|f| {
                f.severity == odsl_core::lint::Severity::Error
                    && !matches!(
                        f.rule,
                        odsl_core::lint::LintRule::MissingTimestamps
                            | odsl_core::lint::LintRule::MissingModelDoc
                    )
            })
            .count();
        problems += hard_errors;
    }

    // 5. Optional live-database sync.
    if let Some(url) = db_url {
        println!("=== live database ({url}) ===");
        let runtime = new_runtime()?;
        let (_, applied) = history_entries(&runtime, url)?;
        let target_lock = odsl_core::lockfile::Lockfile::from_ast(&ast);
        if applied.iter().any(|n| n == &target_lock.checksum) {
            println!("  ✓ current schema checksum is applied");
        } else {
            println!(
                "  ✗ schema checksum {} NOT applied — database is behind or diverged",
                target_lock.checksum
            );
            problems += 1;
        }
    }

    if problems == 0 {
        println!("\n✓ doctor: healthy");
        Ok(())
    } else {
        Err(io_err(format!(
            "doctor found {problems} problem(s) — see report above"
        )))
    }
}
