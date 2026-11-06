use recern_vector::{
    CollectionConfig, Database, Error, HnswParams, Metric, Quantization, SearchOptions,
};
use std::{fs, path::PathBuf};
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "rv-public-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn invalid_hnsw_config_cannot_be_truncated_by_the_file_format() {
    let temp = Temp::new();
    let path = temp.0.join("db.rvec");
    let mut db = Database::create(&path).unwrap();
    if let Some(overflow) = (u32::MAX as usize).checked_add(1) {
        for params in [
            HnswParams {
                ef_search: overflow,
                ..Default::default()
            },
            HnswParams {
                ef_construction: overflow,
                ..Default::default()
            },
        ] {
            assert!(matches!(
                db.create_collection(
                    "bad",
                    CollectionConfig::new(2, Metric::L2).with_hnsw(params)
                ),
                Err(Error::InvalidArgument(_))
            ));
            assert_eq!(db.collections().count(), 0);
        }
    }
    let params = HnswParams {
        ef_search: u32::MAX as usize,
        ef_construction: u32::MAX as usize,
        ..Default::default()
    };
    db.create_collection(
        "valid",
        CollectionConfig::new(2, Metric::L2).with_hnsw(params),
    )
    .unwrap();
    db.checkpoint().unwrap();
    assert_eq!(
        Database::open(&path)
            .unwrap()
            .collection("valid")
            .unwrap()
            .config()
            .hnsw,
        params
    );
}
#[test]
fn finite_components_must_not_produce_infinite_search_distances() {
    let temp = Temp::new();
    let mut db = Database::create(temp.0.join("db.rvec")).unwrap();
    for (index, metric) in [Metric::L2, Metric::Dot, Metric::Cosine]
        .into_iter()
        .enumerate()
    {
        for (q, quantization) in [Quantization::F32, Quantization::Int8]
            .into_iter()
            .enumerate()
        {
            let c = db
                .create_collection(
                    &format!("c{index}{q}"),
                    CollectionConfig::new(2, metric).with_quantization(quantization),
                )
                .unwrap();
            assert!(matches!(
                c.upsert("huge", &[3e38, 0.], None),
                Err(Error::InvalidVector(_))
            ));
            assert!(c.is_empty());
            // A failed batch must also preserve existing data.
            c.upsert("keep", &[1., 0.], None).unwrap();
            assert!(
                c.upsert_many([("new", vec![0., 1.], None), ("huge", vec![3e38, 0.], None)])
                    .is_err()
            );
            assert_eq!(c.len(), 1);
            assert!(
                c.search(&[-3e38, 0.], 1, &SearchOptions::default())
                    .is_err()
            );
            assert!(
                c.search(&[1., 0.], 1, &SearchOptions::default().ef(0))
                    .is_err()
            );
            assert!(
                c.search(&[1., 0.], usize::MAX, &SearchOptions::default().exact())
                    .unwrap()[0]
                    .distance
                    .is_finite()
            );
        }
    }
}
#[test]
#[should_panic(expected = "equal dimensions")]
fn raw_metric_rejects_mismatched_dimensions_in_debug_and_release() {
    Metric::L2.distance(&[1., 2.], &[1.]);
}
