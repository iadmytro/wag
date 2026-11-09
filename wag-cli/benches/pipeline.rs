//! Performance benchmarks for the WAG pipeline.
//!
//! Measures parse, type-check, and full-pipeline throughput on programs
//! of increasing size (N function definitions).
//!
//! Run with: cargo bench -p wag-cli --bench pipeline

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use wag_syntax::{lex, lex_with_layout, parse, parse_desugared};
use wag_types::check as typecheck;
use wag_check::check as check_pass;
use wag_interp::load_checked;

/// Generate a WAG source program with `n` function definitions.
fn generate_program(n: usize) -> String {
    let mut src = String::with_capacity(n * 64);
    for i in 0..n {
        src.push_str(&format!("fn_{i} x = x + {i}\n"));
    }
    src.push_str("main = fn_1 0\n");
    src
}

fn bench_parse(n: usize, criterion: &mut Criterion) {
    let src = generate_program(n);
    let mut group = criterion.benchmark_group(format!("parse_{n}"));
    group.throughput(Throughput::Bytes(src.len() as u64));
    group.sample_size(10);
    group.bench_function("parse", |b| {
        b.iter(|| {
            let _ = parse(&src).expect("parse");
        })
    });
    group.finish();
}

fn bench_typecheck(n: usize, criterion: &mut Criterion) {
    let src = generate_program(n);
    let mut group = criterion.benchmark_group(format!("typecheck_{n}"));
    group.throughput(Throughput::Bytes(src.len() as u64));
    group.sample_size(10);
    group.bench_function("typecheck", |b| {
        b.iter(|| {
            let program = parse(&src).expect("parse");
            let _ = typecheck(&program).expect("typecheck");
        })
    });
    group.finish();
}

fn bench_full_pipeline(n: usize, criterion: &mut Criterion) {
    let src = generate_program(n);
    let mut group = criterion.benchmark_group(format!("pipeline_{n}"));
    group.throughput(Throughput::Bytes(src.len() as u64));
    group.sample_size(10);
    group.bench_function("full", |b| {
        b.iter(|| {
            let program = parse(&src).expect("parse");
            let typed = typecheck(&program).expect("typecheck");
            let checked = check_pass(&program, typed.clone()).expect("check");
            let _ = load_checked(&program, &typed, false);
            let _ = checked;
        })
    });
    group.finish();
}

fn bench_prelude(criterion: &mut Criterion) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../prelude/prelude.wag");
    let src = std::fs::read_to_string(&path).expect("read prelude");
    let mut group = criterion.benchmark_group("prelude");
    group.throughput(Throughput::Bytes(src.len() as u64));
    group.sample_size(10);
    group.bench_function("lex", |b| {
        b.iter(|| {
            let _ = lex(&src).expect("lex");
        })
    });
    group.bench_function("lex_with_layout", |b| {
        b.iter(|| {
            let _ = lex_with_layout(&src).expect("lex_with_layout");
        })
    });
    group.bench_function("parse_desugared", |b| {
        b.iter(|| {
            let _ = parse_desugared(&src).expect("parse_desugared");
        })
    });
    group.bench_function("parse", |b| {
        b.iter(|| {
            let _ = parse(&src).expect("parse");
        })
    });
    group.finish();
}

fn bench(criterion: &mut Criterion) {
    // Real-world: the prelude (675 lines / 119 items) is the pipeline bottleneck.
    bench_prelude(criterion);

    // Small: 100 functions
    bench_parse(100, criterion);
    bench_typecheck(100, criterion);
    bench_full_pipeline(100, criterion);

    // Medium: 1000 functions
    bench_parse(1000, criterion);
    bench_typecheck(1000, criterion);
    bench_full_pipeline(1000, criterion);

    // Large: 10000 functions
    bench_parse(10_000, criterion);
    bench_typecheck(10_000, criterion);
    bench_full_pipeline(10_000, criterion);
}

criterion_group!(benches, bench);
criterion_main!(benches);
