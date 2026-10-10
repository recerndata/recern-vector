use std::path::PathBuf;

use recern_vector::{
    CollectionConfig, Database, Error, Filter, HnswParams, Metric, RecallOptions, SearchOptions,
    Strategy,
};
use serde_json::json;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }

    fn vector(&mut self, dim: usize) -> Vec<f32> {
        (0..dim).map(|_| self.next()).collect()
    }
}

struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "recern-vector-test-{}-{name}.rvec",
            std::process::id()
        ));
        for suffix in ["", ".wal", ".lock"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        Self(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        for suffix in ["", ".wal", ".lock"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

fn memory_db() -> (TempFile, Database) {
    let file = TempFile::new(&format!("{:?}", std::thread::current().id()));
    let db = Database::create(&file.0).unwrap();
    (file, db)
}

#[test]
fn hnsw_reaches_high_recall_on_random_data() {
    let (_file, mut db) = memory_db();
    let c = db
        .create_collection("random", CollectionConfig::new(32, Metric::L2))
        .unwrap();
    let mut rng = Rng(7);
    for i in 0..3000 {
        c.upsert(&i.to_string(), &rng.vector(32), None).unwrap();
    }

    let report = c
        .estimate_recall(&RecallOptions {
            sample: 100,
            k: 10,
            ef_values: vec![16, 128],
            seed: 1,
        })
        .unwrap();
    let (low, high) = (&report.points[0], &report.points[1]);
    assert!(
        high.recall >= 0.95,
        "recall@10 at ef=128 was {}",
        high.recall
    );
    assert!(high.recall >= low.recall);

    let stats = c.stats();
    assert_eq!(stats.live, 3000);
    assert_eq!(stats.unreachable, 0);
    assert_eq!(stats.nodes_per_layer[0], 3000);
    assert!(stats.avg_degree_layer0 <= 32.0);
}

#[test]
fn exact_search_returns_sorted_neighbors_and_finds_itself() {
    let (_file, mut db) = memory_db();
    let c = db
        .create_collection("c", CollectionConfig::new(8, Metric::L2))
        .unwrap();
    let mut rng = Rng(11);
    let vectors: Vec<Vec<f32>> = (0..200).map(|_| rng.vector(8)).collect();
    for (i, v) in vectors.iter().enumerate() {
        c.upsert(&format!("v{i}"), v, None).unwrap();
    }

    let hits = c
        .search(&vectors[42], 5, &SearchOptions::default().exact())
        .unwrap();
    assert_eq!(hits[0].id, "v42");
    assert_eq!(hits[0].distance, 0.0);
    assert!(hits.windows(2).all(|w| w[0].distance <= w[1].distance));

    let ann = c
        .search(&vectors[42], 5, &SearchOptions::default())
        .unwrap();
    assert_eq!(ann, hits);
}

#[test]
fn cosine_normalizes_vectors_and_rejects_invalid_input() {
    let (_file, mut db) = memory_db();
    let c = db
        .create_collection("c", CollectionConfig::new(2, Metric::Cosine))
        .unwrap();
    c.upsert("a", &[3.0, 4.0], None).unwrap();
    let stored = c.get("a").unwrap().vector;
    assert!((stored[0] - 0.6).abs() < 1e-6 && (stored[1] - 0.8).abs() < 1e-6);

    let hit = &c.search(&[6.0, 8.0], 1, &SearchOptions::default()).unwrap()[0];
    assert!(hit.distance.abs() < 1e-6);

    assert!(matches!(
        c.upsert("z", &[0.0, 0.0], None),
        Err(Error::InvalidVector(_))
    ));
    assert!(matches!(
        c.upsert("n", &[f32::NAN, 1.0], None),
        Err(Error::InvalidVector(_))
    ));
    assert!(matches!(
        c.upsert("d", &[1.0, 2.0, 3.0], None),
        Err(Error::DimensionMismatch {
            expected: 2,
            actual: 3
        })
    ));
    assert!(matches!(
        c.search(&[1.0, 0.0], 0, &SearchOptions::default()),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn upsert_replaces_delete_removes_and_compact_reclaims() {
    let (_file, mut db) = memory_db();
    let c = db
        .create_collection("c", CollectionConfig::new(4, Metric::L2))
        .unwrap();
    let mut rng = Rng(3);
    for i in 0..100 {
        c.upsert(&i.to_string(), &rng.vector(4), Some(json!({"v": 1})))
            .unwrap();
    }
    c.upsert("5", &[9.0, 9.0, 9.0, 9.0], Some(json!({"v": 2})))
        .unwrap();
    assert!(c.delete("6"));
    assert!(!c.delete("6"));
    assert!(!c.delete("missing"));

    assert_eq!(c.len(), 99);
    assert_eq!(c.stats().deleted, 2);
    assert_eq!(c.get("5").unwrap().metadata, Some(json!({"v": 2})));
    assert!(c.get("6").is_none());

    let hits = c
        .search(
            &[9.0, 9.0, 9.0, 9.0],
            100,
            &SearchOptions::default().ef(200),
        )
        .unwrap();
    assert_eq!(hits.len(), 99);
    assert_eq!(hits[0].id, "5");
    assert!(hits.iter().all(|h| h.id != "6"));

    assert_eq!(c.compact(), 2);
    assert_eq!(c.compact(), 0);
    let stats = c.stats();
    assert_eq!((stats.live, stats.deleted, stats.unreachable), (99, 0, 0));
    assert_eq!(
        c.search(&[9.0, 9.0, 9.0, 9.0], 1, &SearchOptions::default())
            .unwrap()[0]
            .id,
        "5"
    );
}

#[test]
fn filtered_search_picks_strategy_by_selectivity() {
    let (_file, mut db) = memory_db();
    let c = db
        .create_collection("c", CollectionConfig::new(16, Metric::Cosine))
        .unwrap();
    let mut rng = Rng(5);
    for i in 0..2000 {
        let lang = if i % 2 == 0 { "en" } else { "de" };
        let metadata = json!({"lang": lang, "year": 2000 + i % 25, "rare": i == 1234});
        c.upsert(&i.to_string(), &rng.vector(16), Some(metadata))
            .unwrap();
    }
    let query = rng.vector(16);

    let broad = Filter::from_json(&json!({"lang": "en", "year": {"$gte": 2010}})).unwrap();
    let report = c
        .explain(&query, 10, &SearchOptions::default().filter(broad.clone()))
        .unwrap();
    // Scanning this small matching subset now costs less than widening HNSW.
    assert_eq!(report.strategy, Strategy::FilteredExact);
    assert_eq!(report.hits.len(), 10);
    assert!(
        report
            .hits
            .iter()
            .all(|h| broad.matches(h.metadata.as_ref()))
    );
    let selectivity = report.filter_selectivity.unwrap();
    assert!(
        (0.25..0.45).contains(&selectivity),
        "selectivity {selectivity}"
    );

    let rare = Filter::eq("rare", true);
    let report = c
        .explain(&query, 10, &SearchOptions::default().filter(rare))
        .unwrap();
    assert_eq!(report.strategy, Strategy::FilteredExact);
    assert_eq!(report.hits.len(), 1);
    assert_eq!(report.hits[0].id, "1234");

    let none = Filter::eq("lang", "fr");
    assert!(
        c.search(&query, 10, &SearchOptions::default().filter(none))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn database_round_trips_through_the_file() {
    let file = TempFile::new("roundtrip");
    let mut rng = Rng(9);
    let queries: Vec<Vec<f32>> = (0..5).map(|_| rng.vector(12)).collect();
    let expected = {
        let mut db = Database::create(&file.0).unwrap();
        let config = CollectionConfig::new(12, Metric::Dot).with_hnsw(HnswParams {
            m: 8,
            ef_construction: 64,
            ef_search: 32,
        });
        let c = db.create_collection("docs", config).unwrap();
        for i in 0..500 {
            c.upsert(&format!("doc-{i}"), &rng.vector(12), Some(json!({"i": i})))
                .unwrap();
        }
        c.delete("doc-3");
        db.create_collection("empty", CollectionConfig::new(3, Metric::L2))
            .unwrap();
        db.save().unwrap();
        let c = db.collection("docs").unwrap();
        let results: Vec<_> = queries
            .iter()
            .map(|q| c.search(q, 10, &SearchOptions::default()).unwrap())
            .collect();
        (c.stats(), results)
    };

    let mut db = Database::open(&file.0).unwrap();
    let names: Vec<&str> = db.collections().map(|c| c.name()).collect();
    assert_eq!(names, ["docs", "empty"]);
    let c = db.collection("docs").unwrap();
    assert_eq!(c.stats(), expected.0);
    for (q, hits) in queries.iter().zip(&expected.1) {
        assert_eq!(&c.search(q, 10, &SearchOptions::default()).unwrap(), hits);
    }

    // Inserting after reopening continues with the persisted RNG state.
    db.collection_mut("docs")
        .unwrap()
        .upsert("new", &rng.vector(12), None)
        .unwrap();
    db.save().unwrap();
    assert_eq!(
        Database::open(&file.0)
            .unwrap()
            .collection("docs")
            .unwrap()
            .len(),
        500
    );
}

#[test]
fn rejects_corrupt_and_foreign_files() {
    let file = TempFile::new("corrupt");
    {
        let mut db = Database::create(&file.0).unwrap();
        let c = db
            .create_collection("c", CollectionConfig::new(4, Metric::L2))
            .unwrap();
        c.upsert("a", &[1.0, 2.0, 3.0, 4.0], None).unwrap();
        db.save().unwrap();
    }
    let original = std::fs::read(&file.0).unwrap();

    let mut flipped = original.clone();
    flipped[20] ^= 0xFF;
    std::fs::write(&file.0, &flipped).unwrap();
    assert!(matches!(Database::open(&file.0), Err(Error::Corrupt(_))));

    std::fs::write(&file.0, &original[..original.len() - 7]).unwrap();
    assert!(matches!(Database::open(&file.0), Err(Error::Corrupt(_))));

    let mut newer = original.clone();
    newer[4] = 99;
    std::fs::write(&file.0, &newer).unwrap();
    assert!(matches!(
        Database::open(&file.0),
        Err(Error::UnsupportedVersion(99))
    ));

    std::fs::write(&file.0, b"SQLite format 3\0").unwrap();
    assert!(matches!(Database::open(&file.0), Err(Error::Corrupt(_))));
}

#[test]
fn collection_management_errors() {
    let (file, mut db) = memory_db();
    assert!(matches!(
        Database::create(&file.0),
        Err(Error::FileExists(_))
    ));
    db.create_collection("a", CollectionConfig::new(2, Metric::L2))
        .unwrap();
    assert!(matches!(
        db.create_collection("a", CollectionConfig::new(2, Metric::L2)),
        Err(Error::CollectionExists(_))
    ));
    assert!(matches!(
        db.create_collection("b", CollectionConfig::new(0, Metric::L2)),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        db.collection("missing"),
        Err(Error::CollectionNotFound(_))
    ));
    db.drop_collection("a").unwrap();
    assert!(db.collection("a").is_err());
}

#[test]
fn selectivity_estimate_is_not_fooled_by_periodic_data() {
    let (_file, mut db) = memory_db();
    let c = db
        .create_collection("c", CollectionConfig::new(4, Metric::L2))
        .unwrap();
    let mut rng = Rng(13);
    for i in 0..5000 {
        let lang = ["en", "de", "fr"][i % 3];
        c.upsert(
            &i.to_string(),
            &rng.vector(4),
            Some(json!({ "lang": lang })),
        )
        .unwrap();
    }
    let options = SearchOptions::default().filter(Filter::eq("lang", "de"));
    let report = c.explain(&rng.vector(4), 10, &options).unwrap();
    let selectivity = report.filter_selectivity.unwrap();
    assert!(
        (0.25..0.42).contains(&selectivity),
        "selectivity {selectivity}"
    );
    assert_eq!(report.strategy, Strategy::Hnsw);
    assert_eq!(report.hits.len(), 10);
}

#[test]
fn single_threaded_batch_builds_the_same_graph_as_single_upserts() {
    let mut rng = Rng(21);
    let records: Vec<(String, Vec<f32>)> =
        (0..1500).map(|i| (i.to_string(), rng.vector(16))).collect();
    let (_a, mut one_by_one) = memory_db();
    let (_b, mut batched) = memory_db();
    let config = CollectionConfig::new(16, Metric::L2);
    let a = one_by_one.create_collection("c", config).unwrap();
    let b = batched.create_collection("c", config).unwrap();
    for (id, v) in &records {
        a.upsert(id, v, None).unwrap();
    }
    let written = b
        .upsert_many_with_threads(records.iter().map(|(id, v)| (id.as_str(), v, None)), 1)
        .unwrap();
    assert_eq!(written, 1500);
    assert_eq!(a.stats(), b.stats());
    let query = rng.vector(16);
    let options = SearchOptions::default();
    assert_eq!(
        a.search(&query, 10, &options).unwrap(),
        b.search(&query, 10, &options).unwrap()
    );
}

#[test]
fn parallel_build_keeps_recall_and_reachability() {
    let mut rng = Rng(23);
    let (_file, mut db) = memory_db();
    let c = db
        .create_collection("c", CollectionConfig::new(32, Metric::L2))
        .unwrap();
    let records: Vec<(String, Vec<f32>)> =
        (0..5000).map(|i| (i.to_string(), rng.vector(32))).collect();
    c.upsert_many_with_threads(records.iter().map(|(id, v)| (id.clone(), v, None)), 8)
        .unwrap();
    // A second batch links into an existing graph.
    let more: Vec<(String, Vec<f32>)> = (5000..6000)
        .map(|i| (i.to_string(), rng.vector(32)))
        .collect();
    c.upsert_many(more.iter().map(|(id, v)| (id.clone(), v, None)))
        .unwrap();

    let stats = c.stats();
    assert_eq!((stats.live, stats.deleted, stats.unreachable), (6000, 0, 0));
    assert!(stats.avg_degree_layer0 <= 32.0);
    let report = c
        .estimate_recall(&RecallOptions {
            sample: 200,
            k: 10,
            ef_values: vec![128],
            seed: 3,
        })
        .unwrap();
    assert!(
        report.points[0].recall >= 0.95,
        "recall {}",
        report.points[0].recall
    );
}

#[test]
fn failed_batch_leaves_collection_unchanged() {
    let (_file, mut db) = memory_db();
    let c = db
        .create_collection("c", CollectionConfig::new(2, Metric::Cosine))
        .unwrap();
    c.upsert("keep", &[1.0, 0.0], Some(json!({"v": 1})))
        .unwrap();
    let before = c.stats();

    let batch = vec![
        ("keep", vec![0.0, 1.0], Some(json!({"v": 2}))),
        ("new", vec![1.0, 1.0], None),
        ("bad", vec![0.0, 0.0], None),
    ];
    assert!(matches!(c.upsert_many(batch), Err(Error::InvalidVector(_))));
    assert_eq!(c.stats(), before);
    assert_eq!(c.get("keep").unwrap().metadata, Some(json!({"v": 1})));
    assert!(!c.contains("new"));
}

#[test]
fn duplicate_ids_in_a_batch_keep_the_last_record() {
    let (_file, mut db) = memory_db();
    let c = db
        .create_collection("c", CollectionConfig::new(2, Metric::L2))
        .unwrap();
    let batch = vec![
        ("a", [1.0, 0.0], Some(json!(1))),
        ("b", [0.0, 1.0], None),
        ("a", [5.0, 5.0], Some(json!(2))),
    ];
    assert_eq!(c.upsert_many(batch).unwrap(), 3);
    assert_eq!(c.len(), 2);
    assert_eq!(c.stats().deleted, 1);
    assert_eq!(c.get("a").unwrap().metadata, Some(json!(2)));
}

#[test]
fn cosine_distance_of_a_stored_vector_to_itself_is_not_negative() {
    let file = TempFile::new("self-distance");
    let mut db = Database::create(&file.0).unwrap();
    let c = db
        .create_collection("c", CollectionConfig::new(3, Metric::Cosine))
        .unwrap();
    let v = [0.3, 0.7, 0.2];
    c.upsert("a", &v, None).unwrap();
    for options in [SearchOptions::default(), SearchOptions::default().exact()] {
        let hits = c.search(&v, 1, &options).unwrap();
        assert_eq!(hits[0].id, "a");
        assert!(hits[0].distance >= 0.0, "distance {}", hits[0].distance);
    }
}
