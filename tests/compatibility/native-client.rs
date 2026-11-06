// External Rust consumer of the 0.5 public surface. Keep this source compatible
// with both the released engine and the candidate; regressions have separate tests.
use recern_vector::{CheckpointPolicy, Collection, CollectionConfig, CollectionStats, Database,
    Error, Filter, FORMAT_VERSION, HnswParams, Metric, PersistenceStats, Quantization,
    RecallOptions, RecallPoint, RecallReport, Record, RecordPage, Result, SearchHit,
    SearchOptions, SearchReport, Strategy};
use serde_json::json;
fn send_sync<T: Send + Sync>() {}
fn error_category(e: Error) -> &'static str {
    match e {
        Error::Io(_) => "io", Error::Corrupt(_) => "corrupt",
        Error::UnsupportedVersion(_) => "version", Error::FileExists(_) => "exists",
        Error::CollectionExists(_) => "exists", Error::CollectionNotFound(_) => "missing",
        Error::DimensionMismatch {expected: _, actual: _} => "dimension",
        Error::InvalidVector(_) => "vector", Error::InvalidArgument(_) => "argument",
    }
}
fn main() -> Result<()> {
    send_sync::<Database>(); send_sync::<Collection>();
    let path = std::env::args().nth(1).expect("isolated database path");
    let mut db = Database::create(&path)?;
    let hnsw = HnswParams {m: 8, ef_construction: 64, ef_search: 32};
    let config = CollectionConfig {dim: 2, metric: Metric::L2, hnsw, quantization: Quantization::F32};
    db.set_checkpoint_policy(CheckpointPolicy::new(None, Some(2))?)?;
    let c = db.create_collection("docs", config)?;
    c.upsert("a", &[1., 0.], Some(json!({"kind":"a"})))?;
    c.upsert_many([("b", vec![0., 1.], None)])?;
    c.upsert_many_with_threads([("c", vec![1., 1.], None)], 1)?;
    let record: Record = c.get("a").unwrap();
    assert_eq!(record.vector, vec![1., 0.]);
    let RecordPage {records, next_offset} = c.records_page(0, 1)?;
    assert_eq!(records.len(), 1); assert!(next_offset.is_some());
    let filter = Filter::And(vec![Filter::Or(vec![Filter::eq("kind", "a"),
        Filter::is_in("kind", [json!("b")])]), Filter::Not(Box::new(Filter::between("n", Some(0.), Some(1.))))]);
    assert!(filter.matches(record.metadata.as_ref()));
    let options = SearchOptions {ef: Some(32), exact: true, filter: Some(filter)};
    let SearchReport {hits, strategy, ef: _, visited: _, distance_computations: _, filter_selectivity: _, elapsed: _} = c.explain(&[1., 0.], 1, &options)?;
    let SearchHit {id, distance, metadata: _} = &hits[0];
    assert_eq!(id, "a"); assert_eq!(*distance, 0.);
    match strategy {Strategy::Exact | Strategy::Hnsw | Strategy::FilteredExact => {}}
    c.visit_search(&[1., 0.], 2, &SearchOptions::default().exact(), |_,_,_| Ok(()))?;
    let CollectionStats {name: _, config: _, live, deleted: _, nodes_per_layer: _, avg_degree_layer0: _, unreachable: _, vector_bytes: _, graph_bytes: _, metadata_bytes: _} = c.stats();
    assert_eq!(live, 3);
    let RecallReport {k: _, sample: _, exact_p50: _, points} = c.estimate_recall(&RecallOptions {sample: 2, k: 1, ef_values: vec![8], seed: 1})?;
    let RecallPoint {ef: _, recall, p50: _, p95: _} = points[0]; assert!(recall >= 0.);
    assert!(c.delete("b")); assert_eq!(c.compact(), 1);
    db.save()?; db.checkpoint()?;
    let PersistenceStats {wal_bytes: _, wal_commits, automatic_checkpoints: _, last_checkpoint_error: _, last_encoded_records: _, last_written_pages: _, last_validated_bytes: _} = db.persistence_stats();
    assert_eq!(wal_commits, 0); assert_eq!(db.format_version(), FORMAT_VERSION);
    let ro = Database::open_read_only(&path)?;
    assert!(ro.is_read_only()); assert_eq!(ro.collection("docs")?.len(), 2);
    assert!(ro.save().is_err()); assert_eq!(ro.identity(), db.identity());
    assert_eq!(error_category(Error::CollectionNotFound("missing".into())), "missing");
    println!("Native public Rust consumer passed.");
    Ok(())
}
