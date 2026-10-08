//! Builds an index over synthetic clustered vectors and reports build speed,
//! recall and latency.
//!
//! cargo run --release --example bench -- [records] [dim] [noise]
//!
//! `noise` is the per-dimension spread around cluster centers (default 1.0);
//! higher values make neighbors harder to tell apart.

use std::time::Instant;

use recern_vector::{CollectionConfig, Database, Metric, RecallOptions};

/// Clustered data resembles real embeddings far better than uniform noise.
struct Clusters {
    state: u64,
    noise: f32,
    centers: Vec<Vec<f32>>,
}

impl Clusters {
    fn new(count: usize, dim: usize, noise: f32, seed: u64) -> Self {
        let mut this = Self {
            state: seed,
            noise,
            centers: Vec::new(),
        };
        this.centers = (0..count)
            .map(|_| (0..dim).map(|_| this.gaussian()).collect())
            .collect();
        this
    }

    fn uniform(&mut self) -> f32 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        ((self.state >> 40) as f32 + 1.0) / (1u64 << 24) as f32
    }

    fn gaussian(&mut self) -> f32 {
        let (u, v) = (self.uniform(), self.uniform());
        (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * v).cos()
    }

    fn sample(&mut self) -> Vec<f32> {
        let center = (self.uniform() * self.centers.len() as f32) as usize % self.centers.len();
        let center = self.centers[center].clone();
        center
            .iter()
            .map(|c| c + self.noise * self.gaussian())
            .collect()
    }
}

fn main() -> recern_vector::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| {
        args.get(i)
            .map(|a| a.parse::<f64>().expect("numeric argument"))
    };
    let records = arg(0).map_or(50_000, |v| v as usize);
    let dim = arg(1).map_or(128, |v| v as usize);
    let noise = arg(2).map_or(1.0, |v| v as f32);
    let path = std::env::temp_dir().join("recern-vector-bench.rvec");
    let _ = std::fs::remove_file(&path);

    let mut data = Clusters::new(100, dim, noise, 0x5EED);
    let vectors: Vec<Vec<f32>> = (0..records).map(|_| data.sample()).collect();

    let mut db = Database::create(&path)?;
    let collection = db.create_collection("bench", CollectionConfig::new(dim, Metric::Cosine))?;
    let start = Instant::now();
    for (i, v) in vectors.iter().enumerate() {
        collection.upsert(&i.to_string(), v, None)?;
    }
    let build = start.elapsed();

    let start = Instant::now();
    db.save()?;
    let save = start.elapsed();
    let start = Instant::now();
    let db = Database::open(&path)?;
    let open = start.elapsed();
    let collection = db.collection("bench")?;
    let stats = collection.stats();

    println!("records {records} · dim {dim} · noise {noise} · cosine · m=16 ef_construction=200");
    println!(
        "build   {:.2} s ({:.0} inserts/s)",
        build.as_secs_f64(),
        records as f64 / build.as_secs_f64()
    );
    println!(
        "file    {:.1} MB · save {:.0} ms · open {:.0} ms",
        std::fs::metadata(&path)?.len() as f64 / 1_048_576.0,
        save.as_secs_f64() * 1e3,
        open.as_secs_f64() * 1e3
    );
    println!(
        "graph   layers {:?} · avg degree L0 {:.1} · unreachable {}",
        stats.nodes_per_layer, stats.avg_degree_layer0, stats.unreachable
    );

    let report = collection.estimate_recall(&RecallOptions {
        sample: 200,
        k: 10,
        ef_values: vec![10, 16, 32, 64, 128, 256],
        seed: 7,
    })?;
    println!();
    println!(
        "exact scan p50 {:.3} ms",
        report.exact_p50.as_secs_f64() * 1e3
    );
    println!(
        "{:>6}  {:>9}  {:>10}  {:>10}  {:>9}",
        "ef", "recall@10", "p50 ms", "p95 ms", "speedup"
    );
    for p in &report.points {
        println!(
            "{:>6}  {:>9.3}  {:>10.3}  {:>10.3}  {:>8.0}x",
            p.ef,
            p.recall,
            p.p50.as_secs_f64() * 1e3,
            p.p95.as_secs_f64() * 1e3,
            report.exact_p50.as_secs_f64() / p.p50.as_secs_f64()
        );
    }

    let _ = std::fs::remove_file(&path);
    Ok(())
}
