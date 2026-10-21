//! Prints the build time and retained memory of a `Spec` for a synthetic
//! 2,000-path document. Run with `cargo run --release --example build_cost`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Instant;

#[path = "../benches/support/synthetic.rs"]
mod synthetic;

struct Counting;

static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);

// SAFETY: forwards every call to the system allocator unchanged and only
// records sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let now = LIVE.fetch_add(l.size() as isize, Ordering::Relaxed) + l.size() as isize;
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        LIVE.fetch_sub(l.size() as isize, Ordering::Relaxed);
    }
}

#[global_allocator]
static A: Counting = Counting;

fn mib(b: isize) -> f64 {
    b as f64 / (1024.0 * 1024.0)
}

fn main() {
    let paths: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(2000);
    let chain: usize = std::env::args()
        .nth(2)
        .and_then(|a| a.parse().ok())
        .unwrap_or(4);
    let doc = synthetic::document(paths, chain);
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let t = Instant::now();
    let spec = openapi_validator::Spec::load(doc.as_bytes(), Default::default()).expect("loads");
    let took = t.elapsed();
    let retained = LIVE.load(Ordering::Relaxed) - before;
    let peak = PEAK.load(Ordering::Relaxed) - before;
    println!(
        "document {:.2} MiB, {paths} paths, chain {chain}: build {:.1} ms, retained {:.1} MiB, peak {:.1} MiB",
        doc.len() as f64 / (1024.0 * 1024.0),
        took.as_secs_f64() * 1000.0,
        mib(retained),
        mib(peak)
    );
    let body = synthetic::body(chain.saturating_sub(1));
    let req = openapi_validator::Request {
        method: "POST",
        path: "/v1/resource1999/abc/items",
        headers: &[("Content-Type", "application/json")],
        body: body.as_bytes(),
        ..Default::default()
    };
    let t = Instant::now();
    let r = spec.validate_request(&req);
    println!(
        "first call (compiles): {:.2} ms, valid={}",
        t.elapsed().as_secs_f64() * 1000.0,
        r.is_valid()
    );
    if !r.is_valid() {
        println!("{r}");
    }
    let n = 10_000;
    let t = Instant::now();
    for _ in 0..n {
        std::hint::black_box(spec.validate_request(&req));
    }
    println!(
        "warm call: {:.2} µs",
        t.elapsed().as_secs_f64() * 1e6 / n as f64
    );
    println!(
        "retained after calls: {:.1} MiB",
        mib(LIVE.load(Ordering::Relaxed) - before)
    );
}
