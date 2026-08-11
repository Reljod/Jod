//! Measure the memory graph against the shipped code, not a prototype.
//!
//! `research/sqlite-graph-2026` benchmarked the *design* in Python. This
//! measures the implementation people actually run — through `Store`, with the
//! real migration, real indexes and the undirected traversal, which costs more
//! than the directed one the research timed because it needs two recursive
//! terms rather than one.
//!
//! The graph is scale-free rather than uniform-random, because a memory graph
//! has hubs — `reljod`, `jod`, `linear` — and a traversal from a hub is the
//! case that decides whether this is viable at all. Averages over a uniform
//! graph would flatter it.
//!
//! Run: `cargo run --release --example graph_bench -- [edges]`

use std::io::Write;
use std::time::Instant;

use jod_core::store::{NewFact, Store, DEFAULT_SCOPE};

/// A deterministic generator, so two runs measure the same graph.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*, which is plenty for choosing edge endpoints.
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let at = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[at]
}

/// Time `f` over `runs` iterations and print p50/p95.
///
/// Flushed after every line: redirected stdout is block-buffered, and a
/// benchmark whose progress only appears when it finishes is indistinguishable
/// from one that has hung.
fn measure(label: &str, runs: usize, mut f: impl FnMut(usize) -> usize) {
    print!("{label:<38} ");
    let _ = std::io::stdout().flush();

    let mut times = Vec::with_capacity(runs);
    let mut returned = 0;
    let overall = Instant::now();
    for i in 0..runs {
        let started = Instant::now();
        returned = f(i);
        times.push(started.elapsed().as_secs_f64() * 1000.0);
        // A single pathological query is the finding, not an inconvenience —
        // stop rather than let one case hide behind an average.
        if overall.elapsed().as_secs() > 60 {
            println!("ABANDONED after {} of {runs} runs — >60s", i + 1);
            let _ = std::io::stdout().flush();
            return;
        }
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "p50 {:>9.2} ms   p95 {:>9.2} ms   (last returned {returned})",
        percentile(&times, 0.50),
        percentile(&times, 0.95),
    );
    let _ = std::io::stdout().flush();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let edges: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(100_000);

    // A file rather than `:memory:`, because the question is what happens on
    // the VPS, and page-cache behaviour is part of the answer.
    let dir = std::env::temp_dir().join(format!("jod-graph-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("jod.db");
    let store = Store::open(&path)?;

    // Preferential attachment: an endpoint is chosen from the edges already
    // drawn, so well-connected nodes keep getting more connected.
    println!("building a scale-free graph of {edges} edges…");
    let built = Instant::now();
    let mut rng = Rng(0x5EED);
    let mut endpoints: Vec<usize> = vec![0, 1];
    let mut batch = Vec::with_capacity(5_000);
    let mut nodes = 2usize;

    for i in 0..edges {
        let src = endpoints[rng.below(endpoints.len())];
        // Most edges attach to an existing hub; some introduce a new node, or
        // the graph would never grow past its seed.
        let dst = if rng.below(100) < 30 {
            nodes += 1;
            nodes - 1
        } else {
            endpoints[rng.below(endpoints.len())]
        };
        if src == dst {
            continue;
        }
        endpoints.push(src);
        endpoints.push(dst);
        batch.push(NewFact::new(
            format!("n{src}"),
            "relates-to",
            format!("n{dst}"),
        ));
        if batch.len() == 5_000 || i == edges - 1 {
            store.remember_all(&batch)?;
            batch.clear();
        }
    }

    let (entity_count, relation_count) = store.graph_size()?;
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    println!(
        "built in {:.1}s — {entity_count} entities, {relation_count} relations, {:.1} MB on disk\n",
        built.elapsed().as_secs_f64(),
        size as f64 / 1_048_576.0,
    );

    // The hub is the hard case: the highest-degree node in the graph.
    let hub = {
        let mut counts = std::collections::HashMap::new();
        for e in &endpoints {
            *counts.entry(*e).or_insert(0usize) += 1;
        }
        counts.into_iter().max_by_key(|(_, c)| *c).map(|(n, _)| n).unwrap_or(0)
    };
    println!("hub is n{hub}; random nodes are drawn from the whole graph\n");

    let now = chrono::Utc::now().timestamp_millis();

    for depth in [1u32, 2, 3] {
        measure(&format!("{depth}-hop from a random node"), 20, |i| {
            let n = format!("n{}", (i * 7919) % nodes);
            store.neighbourhood(DEFAULT_SCOPE, &n, depth, now).unwrap().len()
        });
    }

    for depth in [1u32, 2, 3] {
        measure(&format!("{depth}-hop from the hub"), 20, |_| {
            let n = format!("n{hub}");
            store.neighbourhood(DEFAULT_SCOPE, &n, depth, now).unwrap().len()
        });
    }

    // The claim that filtering is free — in fact cheaper, because it prunes
    // edges before expanding them.
    measure("3-hop as of a year ago", 20, |i| {
        let n = format!("n{}", (i * 7919) % nodes);
        let a_year_ago = now - 365 * 24 * 3_600_000;
        store.neighbourhood(DEFAULT_SCOPE, &n, 3, a_year_ago).unwrap().len()
    });

    measure("shortest path, bidirectional BFS", 20, |i| {
        let a = format!("n{}", (i * 7919) % nodes);
        let b = format!("n{}", (i * 104_729) % nodes);
        store
            .path_between(DEFAULT_SCOPE, &a, &b, 6)
            .unwrap()
            .map(|p| p.len())
            .unwrap_or(0)
    });

    measure("hybrid: text seed + 2-hop expansion", 20, |i| {
        let n = format!("n{}", (i * 7919) % nodes);
        store.recall_expanded(DEFAULT_SCOPE, &n, 2, 25).unwrap().len()
    });

    println!("\ndatabase left at {}", path.display());
    Ok(())
}
