use recern_vector_client::{Client, Code, CollectionSpec, HttpOptions, Record, strict_json};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
const FIXTURE: &str = include_str!("../../recern-vector/tests/fixtures/portable-v1.json");
fn shared(client: Client) {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    for metric in ["cosine", "l2", "dot"] {
        let name = format!("collection/世界/{metric}");
        let col = client
            .create_collection(CollectionSpec::new(&name, 2, metric))
            .unwrap();
        assert_eq!(
            client
                .create_collection(CollectionSpec::new(&name, 2, metric))
                .err()
                .unwrap()
                .code,
            Code::AlreadyExists
        );
        let records: Vec<Record> = fixture["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                Record::new(
                    r["id"].as_str().unwrap(),
                    serde_json::from_value(r["vector"].clone()).unwrap(),
                    r["metadata"].clone(),
                )
            })
            .collect();
        assert_eq!(col.upsert_many(&records).unwrap(), records.len());
        assert_eq!(col.count().unwrap(), records.len() as u64);
        for case in fixture["filters"].as_array().unwrap() {
            let hits = col
                .search(&[0.6, 0.8], 20, Some(&case["filter"].to_string()), true)
                .unwrap();
            let ids: std::collections::BTreeSet<_> = hits.iter().map(|h| h.id.as_str()).collect();
            let expected = case["ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert_eq!(ids, expected, "{metric}: {case}");
            for hit in hits {
                let source = records.iter().find(|r| r.id == hit.id).unwrap();
                assert_eq!(
                    strict_json(&hit.metadata_json, 64).unwrap(),
                    source.metadata().unwrap()
                );
                let norm = source
                    .vector
                    .iter()
                    .map(|v| f64::from(*v).powi(2))
                    .sum::<f64>()
                    .sqrt();
                let expected: f64 = match metric {
                    "cosine" => (1.0
                        - source
                            .vector
                            .iter()
                            .zip([0.6, 0.8])
                            .map(|(v, q)| f64::from(*v) / norm * q)
                            .sum::<f64>())
                    .max(0.0),
                    "l2" => source
                        .vector
                        .iter()
                        .zip([0.6, 0.8])
                        .map(|(v, q)| (f64::from(*v) - q).powi(2))
                        .sum(),
                    _ => -source
                        .vector
                        .iter()
                        .zip([0.6, 0.8])
                        .map(|(v, q)| f64::from(*v) * q)
                        .sum::<f64>(),
                };
                assert!((f64::from(hit.distance) - expected).abs() < 2e-6);
            }
        }
        for filter in fixture["invalid_filters"].as_array().unwrap() {
            assert_eq!(
                col.search(&[1.0, 0.0], 10, Some(&filter.to_string()), true)
                    .unwrap_err()
                    .code,
                Code::InvalidArgument
            );
        }
        let mut ids = Vec::new();
        let mut cursor = None;
        loop {
            let page = col.page(2, cursor.as_deref()).unwrap();
            ids.extend(page.records.into_iter().map(|r| r.id));
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        ids.sort();
        let mut expected: Vec<_> = records.iter().map(|r| r.id.clone()).collect();
        expected.sort();
        assert_eq!(ids, expected);
        let before = col.count().unwrap();
        assert_eq!(
            col.upsert_many(&[
                Record::new("valid", vec![1.0, 0.0], Value::Null),
                Record::new("invalid", vec![1.0], Value::Null)
            ])
            .unwrap_err()
            .code,
            Code::InvalidArgument
        );
        assert_eq!(
            col.upsert_many(&[records[0].clone(), records[0].clone()])
                .unwrap_err()
                .code,
            Code::InvalidArgument
        );
        assert_eq!(col.count().unwrap(), before);
        col.upsert(Record {
            id: "numeric".into(),
            vector: vec![1.0, 0.0],
            metadata_json:
                r#"{"u64":18446744073709551615,"i64":-9223372036854775808,"float":[1.0],"int":[1]}"#
                    .into(),
        })
        .unwrap();
        let meta = col.get("numeric").unwrap().unwrap().metadata().unwrap();
        assert_eq!(meta["u64"].as_u64(), Some(u64::MAX));
        assert!(meta["float"][0].is_f64());
        assert!(meta["int"][0].is_u64());
        assert_eq!(
            col.search(&[1.0, 0.0], 10, Some(r#"{"float":[1.0]}"#), true)
                .unwrap()[0]
                .id,
            "numeric"
        );
        assert!(
            col.search(&[1.0, 0.0], 10, Some(r#"{"float":[1]}"#), true)
                .unwrap()
                .is_empty()
        );
        assert!(col.delete("numeric").unwrap());
        assert!(!col.delete("numeric").unwrap());
        assert!(col.get("numeric").unwrap().is_none());
        if client.capabilities().consistent_export {
            let snapshot = client.snapshot().unwrap();
            let frozen = snapshot.collection(&name).unwrap();
            col.upsert(Record::new("after-snapshot", vec![1.0, 0.0], Value::Null))
                .unwrap();
            assert_eq!(frozen.count().unwrap(), before);
            assert_eq!(
                frozen.upsert_many(&[]).unwrap_err().code,
                Code::PermissionDenied
            );
            snapshot.close().unwrap();
            assert_eq!(frozen.count().unwrap_err().code, Code::Unavailable);
        } else {
            assert_eq!(client.snapshot().err().unwrap().code, Code::Unsupported);
        }
        col.drop().unwrap();
        let new = client
            .create_collection(CollectionSpec::new(&name, 2, metric))
            .unwrap();
        assert_ne!(new.descriptor().identity, col.descriptor().identity);
        assert_eq!(col.upsert_many(&[]).unwrap_err().code, Code::Conflict);
        new.drop().unwrap();
    }
    assert!(client.list_collections().unwrap().is_empty());
    client.close().unwrap();
}
#[test]
fn embedded_shared_profile() {
    let dir = tempfile::tempdir().unwrap();
    shared(Client::embedded(dir.path().join("profile.rvec"), false).unwrap());
}
struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[test]
fn remote_shared_profile() {
    let Some(binary) = std::env::var_os("RECERN_TEST_SERVER_BIN") else {
        eprintln!("remote profile requires RECERN_TEST_SERVER_BIN");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let key = "recern-rust-tests-local-only-000000000000";
    std::fs::write(dir.path().join("key"), key).unwrap();
    let log_path = dir.path().join("server.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let mut process = Running(
        Command::new(PathBuf::from(binary))
            .args(["--listen", "127.0.0.1:0", "--database"])
            .arg(dir.path().join("server.rvec"))
            .arg("--api-key-file")
            .arg(dir.path().join("key"))
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let started = Instant::now();
    let url = loop {
        let mut text = String::new();
        std::fs::File::open(&log_path)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        if let Some(endpoint) = text
            .split("listening on ")
            .nth(1)
            .and_then(|s| s.split_once('\n').map(|(line, _)| line.trim()))
        {
            break format!("http://{endpoint}");
        }
        assert!(process.0.try_wait().unwrap().is_none(), "{text}");
        assert!(started.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(20));
    };
    shared(Client::remote(HttpOptions::new(url, Some(key.into()))).unwrap());
}
#[test]
fn qdrant_shared_profile() {
    let Ok(url) = std::env::var("RECERN_TEST_QDRANT_URL") else {
        eprintln!("Qdrant profile requires RECERN_TEST_QDRANT_URL");
        return;
    };
    let options = HttpOptions::new(url, std::env::var("RECERN_TEST_QDRANT_API_KEY").ok());
    shared(
        Client::qdrant(
            options,
            &format!("rust_{}", &uuid::Uuid::new_v4().simple().to_string()[..16]),
        )
        .unwrap(),
    );
}
#[test]
fn strict_json_compatibility() {
    for bad in [
        r#"{"a":1,"a":2}"#,
        r#"{"a":{"x":1,"x":2}}"#,
        "18446744073709551616",
        "-9223372036854775809",
        "1e400",
        r#""\ud800""#,
    ] {
        assert!(strict_json(bad, 64).is_err());
    }
    assert!(strict_json(r#"{"f":1.0}"#, 64).unwrap()["f"].is_f64());
    assert_eq!(
        strict_json(&format!("{}0{}", "[".repeat(65), "]".repeat(65)), 64)
            .unwrap_err()
            .code,
        Code::InvalidArgument
    );
    let _ = json!({"schema":"same frozen profile for every backend"});
}
