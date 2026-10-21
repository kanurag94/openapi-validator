//! Build cost: parse, index and translate a synthetic 2,000-path document.

use criterion::{criterion_group, criterion_main, Criterion};
use openapi_validator::{Options, Spec};

#[path = "support/synthetic.rs"]
mod synthetic;

fn build(c: &mut Criterion) {
    let mut g = c.benchmark_group("build");
    g.sample_size(20);
    for (paths, chain) in [(1200, 4), (2000, 4)] {
        let doc = synthetic::document(paths, chain);
        let name = format!("{paths}_paths_{:.1}MiB", doc.len() as f64 / 1048576.0);
        g.bench_function(name, |b| {
            b.iter(|| Spec::load(std::hint::black_box(doc.as_bytes()), Options::default()).unwrap())
        });
    }
    g.finish();
}

criterion_group!(benches, build);
criterion_main!(benches);
