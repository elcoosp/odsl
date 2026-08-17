//! SLO assertion for the OSDL compiler pipeline (REQ-NFR-PERF-001 / RTM R-04).
//!
//! The full `Parse -> Validate -> Render` pipeline for 100 models must complete
//! in under 500ms. We take the median of several runs to avoid one-off
//! scheduler noise. This runs under `cargo test` (default harness) so a
//! regression fails CI without needing to parse criterion's benchmark report.

use osdl_bench::{SLO_MS, build_schema, run_pipeline};
use osdl_core::intent_compat::Target;

fn median_of(target: Target) -> f64 {
    let src = build_schema(100);
    let mut samples = Vec::new();
    for _ in 0..7 {
        samples.push(run_pipeline(&src, target));
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    samples[samples.len() / 2]
}

#[test]
fn pipeline_seaorm_100_models_under_slo() {
    let median = median_of(Target::SeaOrmSqlite);
    assert!(
        median < SLO_MS,
        "SeaORM pipeline for 100 models took {median:.1}ms (SLO {SLO_MS}ms)"
    );
}

#[test]
fn pipeline_mongo_100_models_under_slo() {
    let median = median_of(Target::Mongo);
    assert!(
        median < SLO_MS,
        "Mongo pipeline for 100 models took {median:.1}ms (SLO {SLO_MS}ms)"
    );
}
