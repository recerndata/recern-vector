use recern_vector::{
    CollectionConfig, Database, Filter, Metric, Quantization, SearchOptions, Strategy,
};
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "rv-v2-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&path).unwrap();
        Self(path.join("test.rvec"))
    }
    fn wal(&self) -> PathBuf {
        PathBuf::from(format!("{}.wal", self.0.display()))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().unwrap());
    }
}
#[test]
fn frozen_v1_migrates_without_changing_graph_or_results() {
    let file = Temp::new();
    fs::write(&file.0, include_bytes!("fixtures/v1-0.0.2.rvec")).unwrap();
    let db = Database::open(&file.0).unwrap();
    let c = db.collection("cosine").unwrap();
    assert_eq!((c.len(), c.stats().deleted), (2, 1));
    assert_eq!(c.get("alpha").unwrap().metadata.unwrap()["lang"], "en");
    assert_eq!(
        c.get("beta").unwrap().metadata.unwrap()["nested"]["tag"],
        "β"
    );
    let hits = c
        .search(&[1., 0., 0.], 2, &SearchOptions::default())
        .unwrap();
    let stats = c.stats();
    db.checkpoint().unwrap();
    assert_eq!(&fs::read(&file.0).unwrap()[4..8], &2u32.to_le_bytes());
    let reopened = Database::open_read_only(&file.0).unwrap();
    assert_eq!(reopened.collection("cosine").unwrap().stats(), stats);
    assert_eq!(
        reopened
            .collection("cosine")
            .unwrap()
            .search(&[1., 0., 0.], 2, &SearchOptions::default())
            .unwrap(),
        hits
    );
    assert!(reopened.save().is_err());
}
#[test]
fn wal_commits_recover_and_checkpoint_is_idempotent() {
    let file = Temp::new();
    let mut db = Database::create(&file.0).unwrap();
    db.create_collection("c", CollectionConfig::new(128, Metric::L2))
        .unwrap()
        .upsert_many_with_threads(
            (0..200).map(|i| (i.to_string(), vec![i as f32; 128], None)),
            1,
        )
        .unwrap();
    db.checkpoint().unwrap();
    let base = fs::read(&file.0).unwrap();
    db.collection_mut("c").unwrap().delete("12");
    db.save().unwrap();
    assert_eq!(fs::read(&file.0).unwrap(), base);
    let log = fs::read(file.wal()).unwrap();
    assert!(log.len() < base.len() / 2, "{} / {}", log.len(), base.len());
    db.save().unwrap();
    assert_eq!(fs::read(file.wal()).unwrap(), log);
    assert!(
        !Database::open_read_only(&file.0)
            .unwrap()
            .collection("c")
            .unwrap()
            .contains("12")
    );
    db.checkpoint().unwrap();
    let checkpoint = fs::read(&file.0).unwrap();
    fs::write(file.wal(), &log).unwrap(); // crash after snapshot rename, before WAL reset
    assert_eq!(
        Database::open(&file.0)
            .unwrap()
            .collection("c")
            .unwrap()
            .len(),
        199
    );
    db.checkpoint().unwrap();
    assert_eq!(fs::read(&file.0).unwrap(), checkpoint);
}
#[test]
fn every_torn_tail_recovers_last_committed_state_and_can_continue() {
    let file = Temp::new();
    let mut db = Database::create(&file.0).unwrap();
    db.create_collection("c", CollectionConfig::new(2, Metric::L2))
        .unwrap()
        .upsert("a", &[1., 2.], None)
        .unwrap();
    db.checkpoint().unwrap();
    db.collection_mut("c")
        .unwrap()
        .upsert("b", &[2., 3.], None)
        .unwrap();
    db.save().unwrap();
    let log = fs::read(file.wal()).unwrap();
    for cut in 0..log.len() {
        fs::write(file.wal(), &log[..cut]).unwrap();
        let recovered = Database::open_read_only(&file.0).unwrap();
        assert_eq!(recovered.collection("c").unwrap().len(), 1, "cut={cut}");
    }
    let mut recovered = Database::open(&file.0).unwrap();
    recovered
        .collection_mut("c")
        .unwrap()
        .upsert("c", &[4., 5.], None)
        .unwrap();
    recovered.save().unwrap();
    assert!(
        Database::open(&file.0)
            .unwrap()
            .collection("c")
            .unwrap()
            .contains("c")
    );
    let mut broken = log;
    let n = broken.len();
    broken[n - 1] ^= 1;
    fs::write(file.wal(), broken).unwrap();
    assert!(Database::open(&file.0).is_err());
}
#[test]
fn concurrent_saves_serialize_and_stale_writers_cannot_overwrite() {
    let file = Temp::new();
    let mut first = Database::create(&file.0).unwrap();
    let stale = Database::open(&file.0).unwrap();
    first
        .create_collection("c", CollectionConfig::new(2, Metric::L2))
        .unwrap();
    let db = Arc::new(first);
    let threads = (0..8)
        .map(|_| {
            let db = db.clone();
            std::thread::spawn(move || {
                for _ in 0..10 {
                    db.save().unwrap();
                }
            })
        })
        .collect::<Vec<_>>();
    for t in threads {
        t.join().unwrap();
    }
    assert!(stale.save().is_err());
    assert_eq!(Database::open(&file.0).unwrap().collections().count(), 1);
}
#[test]
fn int8_round_trips_uses_less_memory_and_keeps_nearest_neighbors() {
    for metric in [Metric::L2, Metric::Cosine, Metric::Dot] {
        let file = Temp::new();
        let mut db = Database::create(&file.0).unwrap();
        let c = db
            .create_collection(
                "q",
                CollectionConfig::new(64, metric).with_quantization(Quantization::Int8),
            )
            .unwrap();
        let mut seed = 7u64;
        let vectors = (0..180)
            .map(|_| {
                (0..64)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        (seed >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        c.upsert_many_with_threads(
            vectors
                .iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), v, None)),
            1,
        )
        .unwrap();
        assert_eq!(c.stats().vector_bytes, 180 * (64 + 4));
        let hits = c
            .search(&vectors[25], 10, &SearchOptions::default().ef(180))
            .unwrap();
        assert_eq!(hits[0].id, "25");
        assert!(
            c.estimate_recall(&recern_vector::RecallOptions {
                sample: 15,
                k: 5,
                ef_values: vec![128],
                seed: 42
            })
            .unwrap()
            .points[0]
                .recall
                > 0.95
        );
        let before = c.get("25").unwrap();
        db.save().unwrap();
        let reopened = Database::open(&file.0).unwrap();
        let c = reopened.collection("q").unwrap();
        assert_eq!(c.get("25").unwrap(), before);
        assert_eq!(
            c.search(&vectors[25], 10, &SearchOptions::default().ef(180))
                .unwrap(),
            hits
        );
        drop(reopened);
        db.collection_mut("q").unwrap().delete("0");
        db.collection_mut("q").unwrap().compact();
        db.checkpoint().unwrap();
        assert_eq!(
            Database::open(&file.0)
                .unwrap()
                .collection("q")
                .unwrap()
                .len(),
            179
        );
    }
}
#[test]
fn logical_filters_missing_fields_validation_and_planning() {
    let f = Filter::from_json(
        &json!({"$or":[{"lang":"en"},{"score":{"$gte":10}}],"$not":{"hidden":true}}),
    )
    .unwrap();
    assert!(f.matches(Some(&json!({"lang":"en"}))));
    assert!(!f.matches(Some(&json!({"lang":"en","hidden":true}))));
    assert!(!f.matches(None));
    assert!(
        Filter::from_json(&json!({"$not":{"missing":null}}))
            .unwrap()
            .matches(None)
    );
    for invalid in [
        json!({"$or":[]}),
        json!({"$not":[]}),
        json!({"$nor":[]}),
        json!({"x":{"$eq":2,"oops":3}}),
    ] {
        assert!(Filter::from_json(&invalid).is_err());
    }
    let file = Temp::new();
    let mut db = Database::create(&file.0).unwrap();
    let c = db
        .create_collection("c", CollectionConfig::new(2, Metric::L2))
        .unwrap();
    c.upsert_many_with_threads(
        (0..2500).map(|i| {
            (
                i.to_string(),
                vec![i as f32, 1.],
                Some(json!({"even":i%2==0})),
            )
        }),
        1,
    )
    .unwrap();
    let opts = SearchOptions::default()
        .filter(Filter::eq("even", true))
        .ef(16);
    let r = c.explain(&[100., 1.], 5, &opts).unwrap();
    assert_eq!(r.strategy, Strategy::Hnsw);
    assert!(r.ef.unwrap() > 16);
    assert_eq!(r.hits[0].id, "100");
    assert!(
        r.hits
            .iter()
            .all(|h| h.metadata.as_ref().unwrap()["even"] == true)
    );
    assert_eq!(
        c.explain(&[100., 1.], 5, &opts.exact()).unwrap().strategy,
        Strategy::Exact
    );
}

#[test]
fn frozen_v2_f32_and_int8_stay_compatible() {
    let file = Temp::new();
    fs::write(&file.0, include_bytes!("fixtures/v2-0.2.0.rvec")).unwrap();
    let db = Database::open(&file.0).unwrap();
    for name in ["f32", "int8"] {
        let c = db.collection(name).unwrap();
        assert_eq!((c.len(), c.stats().deleted), (2, 1));
        assert_eq!(c.config().quantization.as_str(), name);
        assert_eq!(
            c.search(&[1., 0., 0.], usize::MAX, &SearchOptions::default().exact())
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            c.search(&[1., 0., 0.], 1, &SearchOptions::default())
                .unwrap()[0]
                .id,
            "alpha"
        );
        assert_eq!(
            c.get("beta").unwrap().metadata.unwrap()["nested"]["tag"],
            "β"
        );
    }
    let bytes = fs::read(&file.0).unwrap();
    db.checkpoint().unwrap();
    assert_eq!(fs::read(&file.0).unwrap(), bytes);
}

#[test]
fn process_worker() {
    let Some(path) = std::env::var_os("RECERN_WAL_WORKER") else {
        return;
    };
    let mut db = Database::open(path).unwrap();
    db.collection_mut("c")
        .unwrap()
        .upsert("from-process", &[1., 2.], None)
        .unwrap();
    db.save().unwrap();
}
#[test]
fn commits_from_another_process_are_seen_and_stale_writes_rejected() {
    let file = Temp::new();
    let mut db = Database::create(&file.0).unwrap();
    db.create_collection("c", CollectionConfig::new(2, Metric::L2))
        .unwrap();
    db.save().unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "process_worker"])
        .env("RECERN_WAL_WORKER", &file.0)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(db.save().is_err());
    assert!(
        Database::open_read_only(&file.0)
            .unwrap()
            .collection("c")
            .unwrap()
            .contains("from-process")
    );
}

#[test]
fn frozen_wal_replays_and_length_corruption_is_rejected() {
    let file = Temp::new();
    fs::write(&file.0, include_bytes!("fixtures/v2-0.2.0.rvec")).unwrap();
    let original = include_bytes!("fixtures/v2-0.2.0.wal");
    fs::write(file.wal(), original).unwrap();
    assert_eq!(
        Database::open_read_only(&file.0)
            .unwrap()
            .collection("int8")
            .unwrap()
            .get("journal")
            .unwrap()
            .metadata
            .unwrap()["source"],
        "wal"
    );
    let mut corrupted = original.to_vec();
    corrupted[24] ^= 1;
    fs::write(file.wal(), corrupted).unwrap();
    assert!(Database::open_read_only(&file.0).is_err());
}
