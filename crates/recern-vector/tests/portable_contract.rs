use std::collections::BTreeSet;
use std::path::PathBuf;

use recern_vector::{CollectionConfig, Database, Filter, Metric, SearchOptions};
use serde_json::Value;

struct Temp(PathBuf);
impl Temp {
    fn new(label: &str) -> Self {
        Self(std::env::temp_dir().join(format!(
            "recern-portable-{}-{:?}-{label}.rvec",
            std::process::id(),
            std::thread::current().id()
        )))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        for suffix in ["", ".wal", ".lock"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

#[test]
fn shared_portable_v1_filters_match_native_semantics() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/portable-v1.json")).unwrap();
    let file = Temp::new("filters");
    let mut db = Database::create(&file.0).unwrap();
    let col = db
        .create_collection("docs", CollectionConfig::new(2, Metric::Cosine))
        .unwrap();
    for record in fixture["records"].as_array().unwrap() {
        let vector = record["vector"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap() as f32)
            .collect::<Vec<_>>();
        col.upsert(
            record["id"].as_str().unwrap(),
            &vector,
            Some(record["metadata"].clone()),
        )
        .unwrap();
    }
    for case in fixture["filters"].as_array().unwrap() {
        let filter = Filter::from_json(&case["filter"]).unwrap();
        let hits = col
            .search(
                &[0.6, 0.8],
                20,
                &SearchOptions::default().exact().filter(filter),
            )
            .unwrap();
        let actual: BTreeSet<_> = hits.iter().map(|h| h.id.as_str()).collect();
        let expected: BTreeSet<_> = case["ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(actual, expected, "{}", case["filter"]);
    }
    for invalid in fixture["invalid_filters"].as_array().unwrap() {
        assert!(Filter::from_json(invalid).is_err());
    }
}

#[test]
fn bounded_record_pages_skip_replacements_and_tombstones() {
    let file = Temp::new("pages");
    let mut db = Database::create(&file.0).unwrap();
    let id = db.identity();
    assert_ne!(id, 0);
    let col = db
        .create_collection("docs", CollectionConfig::new(2, Metric::L2))
        .unwrap();
    assert!(col.records_page(0, 0).is_err());
    assert!(col.records_page(0, 10_001).is_err());
    assert_eq!(col.records_page(0, 2).unwrap().next_offset, None);
    for n in 0..8 {
        col.upsert(&n.to_string(), &[n as f32, 0.0], None).unwrap();
    }
    col.upsert("1", &[10.0, 0.0], Some(serde_json::json!("updated")))
        .unwrap();
    col.delete("3");
    col.delete("7");
    let mut seen = BTreeSet::new();
    let mut offset = 0;
    loop {
        let page = col.records_page(offset, 2).unwrap();
        assert!(page.records.len() <= 2);
        for record in page.records {
            assert!(seen.insert(record.id.clone()));
            assert_eq!(col.get(&record.id).unwrap(), record);
        }
        match page.next_offset {
            Some(next) => {
                assert!(next > offset);
                offset = next;
            }
            None => break,
        }
    }
    assert_eq!(
        seen,
        ["0", "1", "2", "4", "5", "6"].map(String::from).into()
    );
    assert!(col.records_page(100, 2).is_err());
    db.save().unwrap();
    db.checkpoint().unwrap();
    assert_eq!(Database::open_read_only(&file.0).unwrap().identity(), id);
    let snapshot = Database::open_read_only(&file.0).unwrap();
    db.collection_mut("docs").unwrap().delete("0");
    db.save().unwrap();
    assert_eq!(snapshot.collection("docs").unwrap().len(), 6);
}

#[test]
fn borrowed_search_results_match_owned_hits_and_durable_sequence() {
    let file = Temp::new("borrowed");
    let mut db = Database::create(&file.0).unwrap();
    let initial = db.sequence();
    let col = db
        .create_collection("docs", CollectionConfig::new(2, Metric::Cosine))
        .unwrap();
    for (id, vector) in [("one", [1.0, 0.0]), ("two", [0.0, 1.0])] {
        col.upsert(
            id,
            &vector,
            Some(serde_json::json!({"n":18446744073709551615u64,"f":1.0})),
        )
        .unwrap();
    }
    let options = SearchOptions::default().exact();
    let owned = col.search(&[1.0, 0.0], 2, &options).unwrap();
    let mut borrowed = Vec::new();
    col.visit_search(&[1.0, 0.0], 2, &options, |id, distance, metadata| {
        borrowed.push((id, distance, metadata));
        Ok(())
    })
    .unwrap();
    assert_eq!(owned.len(), borrowed.len());
    for (owned, (id, distance, metadata)) in owned.iter().zip(borrowed) {
        assert_eq!(owned.id, id);
        assert_eq!(owned.distance, distance);
        assert_eq!(owned.metadata.as_ref(), metadata);
    }
    db.save().unwrap();
    assert!(db.sequence() > initial);
    let saved = db.sequence();
    db.save().unwrap();
    assert_eq!(db.sequence(), saved);
    db.checkpoint().unwrap();
    assert_eq!(db.sequence(), saved);
    assert_eq!(Database::open_read_only(&file.0).unwrap().sequence(), saved);
}
