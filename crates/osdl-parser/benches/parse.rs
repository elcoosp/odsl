use criterion::{Criterion, criterion_group, criterion_main};
use osdl_parser::parse;

fn bench_parse(c: &mut Criterion) {
    let src = "User\n  id uuid -pk\n  email string -uniq\n  created_at datetime -tz\nPost\n  id uuid -pk\n  author User.id\n  title string\n  body string -null\n";
    c.bench_function("parse_medium_schema", |b| {
        b.iter(|| parse(criterion::black_box(src)).unwrap())
    });
}

criterion_group!(benches, bench_parse);
criterion_main!(benches);
