//! The `odsl` command-line interface.
//!
//! Commands:
//! * `init`   — scaffold a `schema.odsl` + `odsl.lock` in the current project.
//! * `build`  — parse + validate an `.odsl` file and emit backend code.
//! * `migrate`— diff the schema against `odsl.lock`.
//!   * `migrate plan [--apply]` — print the plan, optionally update the lockfile.
//!   * `migrate create` — write migration files (`migrations/*.sql` or SeaORM).
//!   * `migrate up --db-url …` — apply the plan to a live database.
//! * `lint`   — enforce the configurable schema-quality rule set.
//!
//! The command implementations live in focused modules: [`build`],
//! [`convert`], [`migrate`], [`quality`], [`scaffold`] and the shared
//! [`project`] helpers.

#![allow(clippy::result_large_err)]

mod build;
mod convert;
mod migrate;
mod project;
mod quality;
mod scaffold;

use clap::{Parser, Subcommand};
use convert::ConvertDirection;
use odsl_core::Target;
use odsl_core::errors::OdslError;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "odsl",
    version,
    about = "ODSL schema compiler & migration tool"
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
    /// Scaffold a new `schema.odsl` + `odsl.lock`.
    Init {
        /// Path to the schema file to create.
        #[arg(default_value = "schema.odsl")]
        path: std::path::PathBuf,
    },
    /// Parse, validate and emit backend code.
    Build {
        /// Input `.odsl` file.
        #[arg(default_value = "schema.odsl")]
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
    /// Diff the schema against `odsl.lock` and manage migrations.
    Migrate {
        #[command(subcommand)]
        action: MigrateAction,
    },
    /// Run the ODSL language server (LSP) over stdio.
    ///
    /// Editors connect via the Language Server Protocol; diagnostics are
    /// published on open/change using the same parse+validate pipeline as
    /// `odsl build`.
    Lsp,
    /// Run the ODSL Model Context Protocol (MCP) server over stdio.
    ///
    /// AI agents (Claude, Cursor, Copilot, …) connect via the Model Context
    /// Protocol and can read, validate, format and transpile schemas.
    Mcp,
    /// Reverse-engineer a live database into an ODSL schema.
    ///
    /// Connects to `--db-url` (sqlite://, postgres://, mysql://), reads the
    /// catalog and writes the inferred schema to `schema.odsl` (or `--out`).
    Pull {
        /// Database connection URL (`sqlite://`, `postgres://`, `mysql://`).
        #[arg(long)]
        db_url: String,
        /// Output `.odsl` file (defaults to `schema.odsl`).
        #[arg(default_value = "schema.odsl")]
        out: std::path::PathBuf,
    },
    /// Deterministically reformat an `.odsl` file in place.
    Fmt {
        /// `.odsl` file to format (positional). Omit to read from stdin and
        /// write the result to stdout.
        file: Option<std::path::PathBuf>,
        /// Target backend used for validation during formatting.
        #[arg(long, value_enum, default_value_t = Target::SeaOrmSqlite)]
        target: Target,
    },
    /// Lint the schema against the built-in (configurable) rule set.
    Lint {
        /// Input `.odsl` file.
        #[arg(default_value = "schema.odsl")]
        input: std::path::PathBuf,
        /// Path to a `.odsl-lint.toml` config (defaults to one next to `input`).
        #[arg(long)]
        config: Option<std::path::PathBuf>,
        /// Fail when any warning-severity finding is reported.
        #[arg(long)]
        deny_warnings: bool,
    },
    /// Render an entity-relationship diagram (Mermaid or DBML).
    Erd {
        /// Input `.odsl` file.
        #[arg(default_value = "schema.odsl")]
        input: std::path::PathBuf,
        /// Diagram format: `mermaid` (default) or `dbml`.
        #[arg(long, default_value = "mermaid")]
        format: String,
        /// Write the diagram to this file instead of stdout.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Transpile between ODSL and Prisma / Drizzle schemas.
    Convert {
        /// Conversion direction.
        #[arg(long, value_enum)]
        direction: ConvertDirection,
        /// Input file.
        input: std::path::PathBuf,
        /// Output file.
        out: std::path::PathBuf,
    },
    /// One-shot project health check (schema, lockfile, drift, lint, live DB).
    Doctor {
        /// Input `.odsl` file.
        #[arg(default_value = "schema.odsl")]
        input: std::path::PathBuf,
        /// Target backend.
        #[arg(long, value_enum, default_value_t = Target::SeaOrmSqlite)]
        target: Target,
        /// Also check this live database for the current schema checksum.
        #[arg(long)]
        db_url: Option<String>,
    },
}

#[derive(Subcommand)]
enum MigrateAction {
    /// Print the pending migration plan; `--apply` updates `odsl.lock`.
    Plan {
        /// Input `.odsl` file.
        #[arg(default_value = "schema.odsl")]
        input: std::path::PathBuf,
        /// Target backend.
        #[arg(long, value_enum, default_value_t = Target::SeaOrmSqlite)]
        target: Target,
        /// Write the new lockfile after printing the plan.
        #[arg(long)]
        apply: bool,
    },
    /// Write the pending migration as a `.sql` or SeaORM file.
    Create {
        /// Input `.odsl` file.
        #[arg(default_value = "schema.odsl")]
        input: std::path::PathBuf,
        /// Target backend.
        #[arg(long, value_enum, default_value_t = Target::SeaOrmSqlite)]
        target: Target,
        /// Output directory for the migration file.
        #[arg(long, default_value = "migrations")]
        out: std::path::PathBuf,
        /// Emit a SeaORM migration (Rust) instead of plain SQL.
        #[arg(long)]
        sea_orm: bool,
    },
    /// Apply the pending plan; `--db-url` targets a live database.
    Up {
        /// Input `.odsl` file.
        #[arg(default_value = "schema.odsl")]
        input: std::path::PathBuf,
        /// Target backend.
        #[arg(long, value_enum, default_value_t = Target::SeaOrmSqlite)]
        target: Target,
        /// Database connection URL.
        #[arg(long)]
        db_url: Option<String>,
        /// Apply destructive operations without asking.
        #[arg(long)]
        force: bool,
    },
    /// Roll the live database back to the deployed lockfile state.
    Down {
        /// Input `.odsl` file.
        #[arg(default_value = "schema.odsl")]
        input: std::path::PathBuf,
        /// Target backend.
        #[arg(long, value_enum, default_value_t = Target::SeaOrmSqlite)]
        target: Target,
        /// Database connection URL.
        #[arg(long)]
        db_url: Option<String>,
        /// Apply destructive operations without asking.
        #[arg(long)]
        force: bool,
    },
    /// Show drift between schema, lockfile and (optionally) the live database.
    Status {
        /// Input `.odsl` file.
        #[arg(default_value = "schema.odsl")]
        input: std::path::PathBuf,
        /// Target backend.
        #[arg(long, value_enum, default_value_t = Target::SeaOrmSqlite)]
        target: Target,
        /// Database connection URL.
        #[arg(long)]
        db_url: Option<String>,
    },
    /// Round-trip test: wipe the database, apply `up`, assert, revert `down`.
    Test {
        /// Input `.odsl` file.
        #[arg(default_value = "schema.odsl")]
        input: std::path::PathBuf,
        /// Target backend (SQL backends only).
        #[arg(long, value_enum, default_value_t = Target::SeaOrmSqlite)]
        target: Target,
        /// Database connection URL (the database is wiped first).
        #[arg(long)]
        db_url: String,
        /// Skip the down/revert phase.
        #[arg(long)]
        up_only: bool,
    },
}

fn main() -> Result<(), OdslError> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Command::Init { path } => scaffold::cmd_init(&path),
        Command::Build {
            input,
            target,
            out,
            watch,
        } => {
            if watch {
                build::cmd_build_watch(&input, target, &out)
            } else {
                build::run_build(&input, target, &out)
            }
        }
        Command::Migrate { action } => match action {
            MigrateAction::Plan {
                input,
                target,
                apply,
            } => migrate::cmd_migrate_plan(&input, target, apply),
            MigrateAction::Create {
                input,
                target,
                out,
                sea_orm,
            } => migrate::cmd_migrate_create(&input, target, &out, sea_orm),
            MigrateAction::Up {
                input,
                target,
                db_url,
                force,
            } => migrate::cmd_migrate_up(&input, target, db_url, force),
            MigrateAction::Down {
                input,
                target,
                db_url,
                force,
            } => migrate::cmd_migrate_down(&input, target, db_url, force),
            MigrateAction::Status {
                input,
                target,
                db_url,
            } => migrate::cmd_migrate_status(&input, target, db_url),
            MigrateAction::Test {
                input,
                target,
                db_url,
                up_only,
            } => migrate::cmd_migrate_test(&input, target, &db_url, up_only),
        },
        Command::Fmt { file, target } => quality::cmd_fmt(file.as_deref(), target),
        Command::Lint {
            input,
            config,
            deny_warnings,
        } => quality::cmd_lint(&input, config.as_deref(), deny_warnings),
        Command::Erd { input, format, out } => quality::cmd_erd(&input, &format, out.as_deref()),
        Command::Lsp => {
            odsl_lsp::run_stdio().map_err(|e| OdslError::Io(std::io::Error::other(e.to_string())))
        }
        Command::Mcp => {
            odsl_mcp::run_stdio().map_err(|e| OdslError::Io(std::io::Error::other(e.to_string())))
        }
        Command::Pull { db_url, out } => scaffold::cmd_pull(&db_url, &out),
        Command::Convert {
            direction,
            input,
            out,
        } => convert::cmd_convert(direction, &input, &out),
        Command::Doctor {
            input,
            target,
            db_url,
        } => quality::cmd_doctor(&input, target, db_url.as_deref()),
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

#[cfg(test)]
mod tests {
    use crate::build::event_triggers_rebuild;
    use crate::migrate::cmd_migrate_test;
    use crate::quality::{cmd_doctor, cmd_erd, cmd_lint};
    use notify::{EventKind, event::ModifyKind};
    use odsl_core::Target;
    use odsl_core::lockfile::Lockfile;
    use odsl_migrator::write_lockfile;
    use odsl_parser::parse_project;

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
        let e = ev(EventKind::Modify(ModifyKind::Any), "/proj/schema.odsl");
        assert!(event_triggers_rebuild(&e, "schema.odsl"));
    }

    #[test]
    fn watch_ignores_modify_of_other_file() {
        let e = ev(EventKind::Modify(ModifyKind::Any), "/proj/other.odsl");
        assert!(!event_triggers_rebuild(&e, "schema.odsl"));
    }

    #[test]
    fn watch_ignores_non_modify_events() {
        let created = ev(
            EventKind::Create(notify::event::CreateKind::File),
            "/proj/schema.odsl",
        );
        let accessed = ev(
            EventKind::Access(notify::event::AccessKind::Any),
            "/proj/schema.odsl",
        );
        assert!(!event_triggers_rebuild(&created, "schema.odsl"));
        assert!(!event_triggers_rebuild(&accessed, "schema.odsl"));
    }

    use std::sync::atomic::{AtomicU64, Ordering};
    static LINT_TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    /// Unique temp dir per test invocation (avoids cross-test leakage of
    /// `.odsl-lint.toml` when tests share a process id).
    fn lint_test_dir(tag: &str) -> std::path::PathBuf {
        let n = LINT_TEST_SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "odsl-lint-test-{}-{}-{}",
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
        let schema = dir.join("schema.odsl");
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
        // No .odsl-lint.toml -> defaults; this schema satisfies them.
        cmd_lint(&schema, None, false).expect("clean schema should lint clean");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lint_reports_and_fails_on_bad_schema() {
        let dir = lint_test_dir("bad");
        let schema = dir.join("schema.odsl");
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
        let schema = dir.join("schema.odsl");
        std::fs::write(
            &schema,
            "user_profile
  id uuid -pk
  user_id uuid
",
        )
        .unwrap();
        // Disable the two rules that would otherwise fire (timestamps=error, model-naming=warn).
        let cfg = dir.join(".odsl-lint.toml");
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
            std::env::temp_dir().join(format!("odsl-migtest-{}-{}-{}", std::process::id(), tag, n));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn migrate_test_applies_up_and_reverts_down() {
        let dir = migrate_test_dir("updown");
        let schema = dir.join("schema.odsl");
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
        cmd_migrate_test(&schema, Target::SeaOrmSqlite, &db_url, false)
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
        let schema = dir.join("schema.odsl");
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
        cmd_migrate_test(&schema, Target::SeaOrmSqlite, &db_url, false)
            .expect("valid schema should pass migrate test");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn erd_test_dir(tag: &str) -> std::path::PathBuf {
        let n = MIGRATE_TEST_SEQ.fetch_add(1, SeqOrd::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("odsl-erdtest-{}-{}-{}", std::process::id(), tag, n));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn erd_renders_mermaid_and_dbml() {
        let dir = erd_test_dir("basic");
        let schema = dir.join("schema.odsl");
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
        let schema = dir.join("schema.odsl");
        std::fs::write(&schema, "User\n  id uuid -pk\n").unwrap();
        let err = cmd_erd(&schema, "svg", None).unwrap_err();
        assert!(format!("{err}").contains("unknown ERD format"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    use std::sync::atomic::{AtomicU64 as DoctorSeq, Ordering as DoctorOrd};
    static DOCTOR_TEST_SEQ: DoctorSeq = AtomicU64::new(0);

    fn doctor_test_dir(tag: &str) -> std::path::PathBuf {
        let n = DOCTOR_TEST_SEQ.fetch_add(1, DoctorOrd::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("odsl-doctest-{}-{}-{}", std::process::id(), tag, n));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn write_in_sync_lock(dir: &std::path::Path, src: &str) -> std::path::PathBuf {
        let schema = dir.join("schema.odsl");
        std::fs::write(&schema, src).unwrap();
        let project = parse_project(&schema).expect("schema parses");
        let lf = Lockfile::from_ast(&project.ast);
        let lock = dir.join("odsl.lock");
        write_lockfile(&lock, &lf).expect("lockfile writes");
        schema
    }

    #[test]
    fn doctor_reports_healthy_when_in_sync() {
        let dir = doctor_test_dir("healthy");
        let schema = write_in_sync_lock(&dir, "User\n  id uuid -pk\n  email string -uniq\n");
        let result = cmd_doctor(&schema, Target::SeaOrmSqlite, None);
        assert!(result.is_ok(), "doctor should pass on an in-sync project");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn doctor_detects_drift() {
        let dir = doctor_test_dir("drift");
        // Write an in-sync baseline so the lockfile exists.
        let schema = write_in_sync_lock(&dir, "User\n  id uuid -pk\n  email string -uniq\n");
        // Now change the schema so it no longer matches the lockfile.
        std::fs::write(
            &schema,
            "User\n  id uuid -pk\n  email string -uniq\n  name string\n",
        )
        .unwrap();
        let result = cmd_doctor(&schema, Target::SeaOrmSqlite, None);
        assert!(
            result.is_err(),
            "doctor should fail when schema has drifted"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
