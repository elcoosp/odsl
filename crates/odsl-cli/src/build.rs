//! `odsl build` — parse, validate and emit backend code (optionally watching
//! the schema file for changes).

use crate::project::{io_err, load_ast, now_str};
use odsl_codegen_graphql::GraphQLRenderer;
use odsl_codegen_jsonschema::JsonSchemaRenderer;
use odsl_codegen_mongo::MongoRenderer;
use odsl_codegen_openapi::OpenApiRenderer;
use odsl_codegen_seaorm::SeaOrmRenderer;
use odsl_codegen_trpc::TrpcRenderer;
use odsl_codegen_ts_validators::{TsValidatorRenderer, ValidatorFlavor};
use odsl_codegen_typescript::TypeScriptRenderer;
use odsl_core::Target;
use odsl_core::errors::OdslError;
use odsl_core::validator::CodeRenderer;

/// Build (parse + validate + codegen) once. Returns `Ok` on success.
pub fn run_build(
    input: &std::path::Path,
    target: Target,
    out: &std::path::Path,
) -> Result<(), OdslError> {
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
        Target::Trpc => TrpcRenderer::new(target).render(&ast)?,
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

/// Watch `input` for changes and rebuild on every modification until the
/// process is interrupted (Ctrl-C). Uses the `notify` crate's OS-native file
/// watcher; the first build runs immediately.
pub fn cmd_build_watch(
    input: &std::path::Path,
    target: Target,
    out: &std::path::Path,
) -> Result<(), OdslError> {
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
        .unwrap_or_else(|| "*.odsl".to_string());

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
pub fn event_triggers_rebuild(event: &notify::Event, watch_name: &str) -> bool {
    let is_modify = matches!(event.kind, notify::EventKind::Modify(_));
    let relevant = event.paths.iter().any(|p| {
        p.file_name()
            .map(|n| n.to_string_lossy() == watch_name)
            .unwrap_or(false)
    });
    is_modify && relevant
}
