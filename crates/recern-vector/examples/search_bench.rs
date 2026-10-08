//! Search latency on a saved index, without Python.
//!
//! cargo run --release --example search_bench -- <index.rvec> <queries.f32> <truth.u32> [ef,...]
//!
//! `queries.f32` holds row-major float32 query vectors; `truth.u32` the ids
//! of the 10 true nearest neighbors of each query (record ids are their row
//! numbers as strings, as written by bench/ann_bench.py).

use std::time::Instant;

use recern_vector::{Database, SearchOptions};

fn read<T: Copy>(path: &str, from: fn([u8; 4]) -> T) -> Vec<T> {
    std::fs::read(path)
        .expect(path)
        .chunks_exact(4)
        .map(|b| from(b.try_into().unwrap()))
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let db = Database::open(&args[0]).expect("index");
    let c = db.collections().next().expect("a collection");
    let dim = c.config().dim;
    let queries = read(&args[1], f32::from_le_bytes);
    let truth = read(&args[2], u32::from_le_bytes);
    let efs: Vec<usize> = args
        .get(3)
        .map_or("10,20,40,80,160", String::as_str)
        .split(',')
        .map(|e| e.parse().unwrap())
        .collect();
    let n = queries.len() / dim;

    println!(
        "{:>5}  {:>8}  {:>9}  {:>9}  {:>10}",
        "ef", "recall", "mean µs", "p50 µs", "distances"
    );
    for ef in efs {
        let options = SearchOptions::default().ef(ef);
        let (mut times, mut found, mut distances) = (Vec::with_capacity(n), 0, 0);
        for (q, t) in queries.chunks_exact(dim).zip(truth.chunks_exact(10)) {
            let start = Instant::now();
            let report = c.explain(q, 10, &options).unwrap();
            times.push(start.elapsed().as_secs_f64() * 1e6);
            distances += report.distance_computations;
            found += report
                .hits
                .iter()
                .filter(|h| t.contains(&h.id.parse::<u32>().unwrap()))
                .count();
        }
        let mean = times.iter().sum::<f64>() / n as f64;
        times.sort_by(f64::total_cmp);
        println!(
            "{ef:>5}  {:>8.4}  {mean:>9.1}  {:>9.1}  {:>10}",
            found as f64 / (n * 10) as f64,
            times[n / 2],
            distances / n
        );
    }
}
