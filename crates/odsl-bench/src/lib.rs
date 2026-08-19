//! Shared helpers for the ODSL compiler performance benchmarks.
//!
//! Exposes the synthetic-schema builder and the full `Parse -> Validate ->
//! Render` pipeline timer used by both the `criterion` benches and the SLO
//! integration tests (REQ-NFR-PERF-001 / RTM R-04: p99 < 500ms for 100 models).

use odsl_codegen_mongo::MongoRenderer;
use odsl_codegen_seaorm::SeaOrmRenderer;
use odsl_core::intent_compat::Target;
use odsl_core::validator::{CodeRenderer, Validator};
use odsl_parser::parse;

/// SLO from REQ-NFR-PERF-001: p99 < 500ms for 100 models.
pub const SLO_MS: f64 = 500.0;

/// Build a synthetic schema with `n` models, each with a PK, a unique string,
/// a nullable text field, and a reference to the previous model (so reference
/// resolution and the validator actually do work).
pub fn build_schema(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        let name = format!("Model{i}");
        let ref_field = if i == 0 {
            String::new()
        } else {
            format!("  parent Model{}.id -null\n", i - 1)
        };
        s.push_str(&name);
        s.push('\n');
        s.push_str("  id uuid -pk\n");
        s.push_str("  name string -uniq\n");
        s.push_str("  body text -null\n");
        s.push_str(&ref_field);
    }
    s
}

/// Run the full pipeline once and return the elapsed milliseconds.
pub fn run_pipeline(src: &str, target: Target) -> f64 {
    let start = std::time::Instant::now();
    let ast = parse(src).expect("synthetic schema must parse");
    Validator::validate(&ast, Some(target)).expect("synthetic schema must validate");
    let _ = match target {
        Target::Mongo => MongoRenderer::new(target).render(&ast),
        _ => SeaOrmRenderer::new(target).render(&ast),
    }
    .expect("rendering must succeed");
    start.elapsed().as_secs_f64() * 1000.0
}
