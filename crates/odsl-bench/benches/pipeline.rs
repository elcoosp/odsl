//! Criterion benchmark for the ODSL compiler pipeline (REQ-NFR-PERF-001).
//!
//! Benchmarks the full `Parse -> Validate -> Render` path on a synthetic
//! 100-model schema for both the SeaORM (SQLite) and MongoDB targets. Run with
//! `cargo bench -p odsl-bench`. The SLO assertion lives in `tests/slo.rs` so it
//! runs under `cargo test` (default harness) and fails CI on regression.

use criterion::{Criterion, criterion_group, criterion_main};
use odsl_bench::{build_schema, run_pipeline};
use odsl_core::intent_compat::Target;

fn bench_seaorm(c: &mut Criterion) {
    let src = build_schema(100);
    c.bench_function("pipeline_seaorm_100_models", |b| {
        b.iter(|| {
            let _ = run_pipeline(std::hint::black_box(&src), Target::SeaOrmSqlite);
        })
    });
}

fn bench_mongo(c: &mut Criterion) {
    let src = build_schema(100);
    c.bench_function("pipeline_mongo_100_models", |b| {
        b.iter(|| {
            let _ = run_pipeline(std::hint::black_box(&src), Target::Mongo);
        })
    });
}

criterion_group!(benches, bench_seaorm, bench_mongo);
criterion_main!(benches);
