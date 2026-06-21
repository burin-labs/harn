//! Latency ladder + profiling harness for the embedding core.
//!
//! Reports, for the lexical (default, zero-asset) backend and — when a
//! static asset is resolvable — the Model2Vec-style static backend:
//!
//!   (a) single-embed latency (p50/p99) at a representative input length;
//!   (b) top-k cosine lookup latency (p50/p99) across corpus sizes
//!       10 / 100 / 1k / 10k;
//!   (c) cold-start cost (first embed after construction);
//!   (d) approximate peak resident memory of the precomputed corpus
//!       vectors.
//!
//! Run (isolated target dir avoids colliding with concurrent evals):
//!
//! ```sh
//! CARGO_TARGET_DIR=/private/tmp/embedder-cargo \
//!   cargo run -p harn-hostlib --release --example embed_latency_ladder
//! ```
//!
//! Optionally point at a static asset dir to benchmark that backend too:
//! `EMBED_STATIC_DIR=/path/to/asset cargo run ... --example embed_latency_ladder`.
//!
//! This is a deliberately dependency-light harness (no criterion) so it can
//! be run ad hoc on any platform and prints a copy-pasteable table. The
//! numbers below are wall-clock on the host that ran it; Linux/Windows are
//! expected to be in the same order of magnitude since the default backend
//! is pure integer/float arithmetic with no platform-specific code.

use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

use harn_hostlib::embed::{top_k, Embedder, LexicalEmbedder, StaticEmbedder};

const CORPUS_SIZES: &[usize] = &[10, 100, 1_000, 10_000];
const SINGLE_EMBED_ITERS: usize = 50_000;
const TOPK_ITERS: usize = 2_000;

fn percentile(sorted_nanos: &[u128], pct: f64) -> u128 {
    if sorted_nanos.is_empty() {
        return 0;
    }
    let rank = ((pct / 100.0) * (sorted_nanos.len() as f64 - 1.0)).round() as usize;
    sorted_nanos[rank.min(sorted_nanos.len() - 1)]
}

fn synth_text(i: usize) -> String {
    // Code-ish phrases: identifiers + a short description, the real input
    // shape (symbol names, task descriptions, skill/canon snippets).
    let verbs = [
        "handle", "parse", "render", "validate", "resolve", "compute",
    ];
    let nouns = [
        "rate limiter",
        "markdown table",
        "json payload",
        "auth token",
        "retry backoff",
        "symbol graph",
    ];
    format!(
        "{}_{} for the {} module #{}",
        verbs[i % verbs.len()],
        nouns[i % nouns.len()].replace(' ', "_"),
        nouns[(i / 7) % nouns.len()],
        i
    )
}

fn bench_backend(label: &str, embedder: &dyn Embedder) {
    println!("\n=== backend: {label} (dim={}) ===", embedder.dim());

    // --- cold start: first embed after construction ---
    let cold = Instant::now();
    let _ = black_box(embedder.embed("cold start probe input"));
    println!(
        "cold-start single embed:        {:>8.1} us",
        cold.elapsed().as_nanos() as f64 / 1000.0
    );

    // --- single-embed latency ladder ---
    let sample = synth_text(42);
    let mut samples: Vec<u128> = Vec::with_capacity(SINGLE_EMBED_ITERS);
    // warm
    for _ in 0..1000 {
        black_box(embedder.embed(&sample));
    }
    for _ in 0..SINGLE_EMBED_ITERS {
        let t = Instant::now();
        black_box(embedder.embed(black_box(&sample)));
        samples.push(t.elapsed().as_nanos());
    }
    samples.sort_unstable();
    println!(
        "single embed:                   p50 {:>7.2} us   p99 {:>7.2} us",
        percentile(&samples, 50.0) as f64 / 1000.0,
        percentile(&samples, 99.0) as f64 / 1000.0
    );

    // --- top-k cosine across corpus sizes ---
    let query = embedder.embed("rate limiter middleware for the api");
    for &size in CORPUS_SIZES {
        let texts: Vec<String> = (0..size).map(synth_text).collect();
        let build = Instant::now();
        let corpus: Vec<Vec<f32>> = embedder.embed_batch(&texts);
        let build_ms = build.elapsed().as_secs_f64() * 1000.0;
        // approx peak memory of precomputed corpus vectors
        let mem_kb = (size * embedder.dim() * std::mem::size_of::<f32>()) as f64 / 1024.0;

        let mut tk: Vec<u128> = Vec::with_capacity(TOPK_ITERS);
        for _ in 0..TOPK_ITERS {
            let t = Instant::now();
            black_box(top_k(black_box(&query), black_box(&corpus), 10));
            tk.push(t.elapsed().as_nanos());
        }
        tk.sort_unstable();
        println!(
            "top-10 over {:>6} items:       p50 {:>7.2} us   p99 {:>7.2} us   (corpus build {:>7.2} ms, vecs ~{:>8.1} KB)",
            size,
            percentile(&tk, 50.0) as f64 / 1000.0,
            percentile(&tk, 99.0) as f64 / 1000.0,
            build_ms,
            mem_kb,
        );
    }
}

fn main() {
    println!("embedding-core latency ladder");
    println!(
        "host: {} / {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );

    let lexical = LexicalEmbedder::default();
    bench_backend("lexical-hash (default, zero-asset)", &lexical);

    if let Ok(dir) = std::env::var("EMBED_STATIC_DIR") {
        match StaticEmbedder::from_asset_dir(&PathBuf::from(&dir)) {
            Ok(s) => bench_backend("static-model2vec", &s),
            Err(e) => eprintln!("\n(static backend skipped: {e})"),
        }
    } else {
        println!("\n(set EMBED_STATIC_DIR=<asset dir> to also benchmark the static backend)");
    }
}
