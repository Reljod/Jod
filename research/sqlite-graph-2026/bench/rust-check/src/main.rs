//! Confirm the decisive numbers on the engine Jod actually ships.
//!
//! The Python sweep ran on the system SQLite (3.46.1). Jod links
//! `rusqlite = { features = ["bundled"] }`, which is SQLite 3.50.2 compiled
//! into the binary. A recommendation that rests on recursive-CTE latency
//! should be checked against the version that will run it.
//!
//!     cargo run --release -- /path/to/g100k.db

use std::time::Instant;

use rusqlite::{Connection, OpenFlags};

const KHOP_DIRECTED: &str = "
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT e.dst, r.depth + 1
    FROM reach r
    JOIN edges e ON e.src = r.node AND e.scope = ?3
   WHERE r.depth < ?2
)
SELECT node, MIN(depth) AS hops FROM reach WHERE node <> ?1 GROUP BY node";

const KHOP_UNDIRECTED: &str = "
WITH RECURSIVE reach(node, depth) AS (
  SELECT ?1, 0
  UNION
  SELECT e.dst, r.depth + 1
    FROM reach r JOIN edges e ON e.src = r.node AND e.scope = ?3
   WHERE r.depth < ?2
  UNION
  SELECT e.src, r.depth + 1
    FROM reach r JOIN edges e ON e.dst = r.node AND e.scope = ?3
   WHERE r.depth < ?2
)
SELECT node, MIN(depth) AS hops FROM reach WHERE node <> ?1 GROUP BY node";

fn pct(xs: &mut Vec<f64>, p: f64) -> f64 {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let k = (((xs.len() - 1) as f64) * p / 100.0).round() as usize;
    (xs[k] * 1000.0).round() / 1000.0
}

fn main() -> rusqlite::Result<()> {
    let path = std::env::args().nth(1).expect("usage: jod-graph-check DB");
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.pragma_update(None, "busy_timeout", 5000)?;

    let version: String =
        conn.query_row("SELECT sqlite_version()", [], |r| r.get(0))?;
    let edges: i64 =
        conn.query_row("SELECT count(*) FROM edges", [], |r| r.get(0))?;
    let ids: Vec<i64> = conn
        .prepare("SELECT id FROM nodes")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;

    println!("sqlite {version}  edges {edges}  nodes {}", ids.len());

    // Deterministic pseudo-random seeds, so a re-run compares like with like.
    let seeds: Vec<i64> = (0..120)
        .map(|i| ids[(i * 7919) % ids.len()])
        .collect();

    for (name, sql) in [
        ("directed", KHOP_DIRECTED),
        ("undirected", KHOP_UNDIRECTED),
    ] {
        for k in [1i64, 2, 3, 4] {
            let mut stmt = conn.prepare(sql)?;
            let mut lat = Vec::with_capacity(seeds.len());
            let mut rows_total = 0usize;
            for &s in &seeds {
                let t0 = Instant::now();
                let n = stmt
                    .query_map(rusqlite::params![s, k, "default"], |r| {
                        r.get::<_, i64>(0)
                    })?
                    .count();
                lat.push(t0.elapsed().as_secs_f64() * 1000.0);
                rows_total += n;
            }
            let mean_rows = rows_total as f64 / seeds.len() as f64;
            let (p50, p95) = (pct(&mut lat, 50.0), pct(&mut lat, 95.0));
            println!(
                "{name}.k{k}: p50 {p50} ms  p95 {p95} ms  mean_rows {mean_rows:.1}"
            );
        }
    }
    Ok(())
}
