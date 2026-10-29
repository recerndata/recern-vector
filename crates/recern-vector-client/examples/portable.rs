//! Same application logic, three configurations. Keys are read from the
//! environment rather than command-line arguments or log messages.
use recern_vector_client::{Client, CollectionSpec, HttpOptions, Record};
use serde_json::{Value, json};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let option = |name: &str| {
        args.iter()
            .position(|s| s == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let mut http = HttpOptions::new(
        option("--remote")
            .or_else(|| option("--qdrant"))
            .unwrap_or_default(),
        std::env::var("RECERN_API_KEY").ok(),
    );
    http.ca_file = option("--ca").map(Into::into);
    let client = if option("--remote").is_some() {
        Client::remote(http)?
    } else if option("--qdrant").is_some() {
        Client::qdrant(http, &option("--namespace").unwrap_or("example".into()))?
    } else {
        Client::embedded(option("--path").unwrap_or("portable.rvec".into()), false)?
    };
    if args.iter().any(|s| s == "--check") {
        let col = client.collection("interop")?;
        let record = col
            .get("python/東京\n\0")?
            .ok_or("missing interop record")?;
        let meta = record.metadata()?;
        if meta["u64"].as_u64() != Some(u64::MAX)
            || !meta["float"][0].is_f64()
            || !meta["integer"][0].is_u64()
        {
            return Err("metadata was not preserved".into());
        }
        col.upsert(Record {
            id: "rust".into(),
            vector: vec![1.0, 0.0],
            metadata_json: record.metadata_json,
        })?;
        println!(
            "{}",
            json!({"verified":true,"count":col.count()?.to_string()})
        );
    } else {
        let col = client.create_collection(CollectionSpec::new("documents", 2, "cosine"))?;
        col.upsert_many(&[
            Record::new("first", vec![1.0, 0.0], json!({"lang":"en"})),
            Record::new("second", vec![0.0, 1.0], Value::Null),
        ])?;
        let hits = col.search(&[1.0, 0.0], 5, Some(r#"{"lang":"en"}"#), true)?;
        println!(
            "{}",
            json!({"backend":client.capabilities().backend,"count":col.count()?.to_string(),"hits":hits})
        );
    }
    client.close()?;
    Ok(())
}
