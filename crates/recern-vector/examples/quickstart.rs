//! Recern Vector from Rust: create a database, insert records with metadata,
//! search with a filter, see how the search ran, then save and reopen.
//!
//!     cargo run --release --example quickstart

use recern_vector::{CollectionConfig, Database, Filter, Metric, RecallOptions, SearchOptions};
use serde_json::json;

fn main() -> recern_vector::Result<()> {
    let path = std::env::temp_dir().join("recern-vector-quickstart.rvec");
    let _ = std::fs::remove_file(&path);

    let mut db = Database::create(&path)?;
    let docs = db.create_collection("docs", CollectionConfig::new(32, Metric::Cosine))?;

    // Batch insert: (id, vector, metadata). Built on all cores, atomic.
    let records: Vec<_> = (0..10_000)
        .map(|i| {
            let vector: Vec<f32> = (0..32).map(|d| pseudo_random(i * 32 + d)).collect();
            let lang = ["en", "de", "fr"][i % 3];
            (
                format!("doc-{i}"),
                vector,
                Some(json!({"lang": lang, "year": 2015 + i % 10})),
            )
        })
        .collect();
    let query = records[42].1.clone();
    docs.upsert_many(records)?;
    println!("{} records", docs.len());

    // Search with a filter; explain() returns the hits and how they were found.
    let filter = Filter::from_json(&json!({"lang": "en", "year": {"$gte": 2020}}))?;
    // The same filter, built in code:
    let _same = Filter::And(vec![
        Filter::eq("lang", "en"),
        Filter::between("year", Some(2020.0), None),
    ]);
    let report = docs.explain(&query, 3, &SearchOptions::default().filter(filter))?;
    for hit in &report.hits {
        println!(
            "  {:<10} distance={:.4} {}",
            hit.id,
            hit.distance,
            hit.metadata.as_ref().unwrap()
        );
    }
    println!(
        "strategy={} visited={} selectivity={:.1}% time={:?}",
        report.strategy,
        report.visited,
        report.filter_selectivity.unwrap_or(1.0) * 100.0,
        report.elapsed
    );

    // Index health and recall against exact search.
    let stats = docs.stats();
    println!(
        "layers={:?} unreachable={}",
        stats.nodes_per_layer, stats.unreachable
    );
    let recall = docs.estimate_recall(&RecallOptions {
        ef_values: vec![16, 64],
        ..Default::default()
    })?;
    for point in &recall.points {
        println!(
            "  ef={:<3} recall@{}={:.3}",
            point.ef, recall.k, point.recall
        );
    }

    db.save()?;
    let reopened = Database::open(&path)?;
    println!("reopened: {} records", reopened.collection("docs")?.len());
    Ok(())
}

/// A tiny deterministic generator so the example has no dependencies.
fn pseudo_random(seed: usize) -> f32 {
    let mut x = seed as u64 ^ 0x9E37_79B9_7F4A_7C15;
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    ((x ^ (x >> 31)) as f64 / u64::MAX as f64 * 2.0 - 1.0) as f32
}
