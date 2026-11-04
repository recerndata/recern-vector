use recern_vector::{Database, SearchOptions};
use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "rv-v3-frozen-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
        Self(dir.join("fixture.rvec"))
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
fn frozen_v3_snapshot_and_wal_preserve_precisions_chunks_graph_and_sequence() {
    let file = Temp::new();
    let snapshot = include_bytes!("fixtures/v3-0.5.0-dev.1.rvec");
    let journal = include_bytes!("fixtures/v3-0.5.0-dev.1.wal");
    fs::write(&file.0, snapshot).unwrap();
    let ro = Database::open_read_only(&file.0).unwrap();
    assert_eq!(ro.format_version(), 3);
    assert_eq!(ro.sequence(), 1);
    assert_eq!(ro.collection("empty").unwrap().len(), 0);
    let stats = ro.collection("int8").unwrap().stats();
    for name in ["f32", "int8"] {
        let c = ro.collection(name).unwrap();
        assert_eq!((c.len(), c.stats().deleted), (2, 1));
        let b = c.get("beta").unwrap().metadata.unwrap();
        assert_eq!(b["nested"]["tag"], "β");
        assert_eq!(b["large"].as_str().unwrap().len(), 6000);
        assert_eq!(
            c.search(&[1., 0., 0.], 1, &SearchOptions::default())
                .unwrap()[0]
                .id,
            "alpha"
        );
    }
    fs::write(file.wal(), journal).unwrap();
    let db = Database::open(&file.0).unwrap();
    assert_eq!(db.sequence(), 2);
    assert_eq!(db.persistence_stats().wal_commits, 1);
    assert_eq!(db.collection("int8").unwrap().len(), 3);
    assert_eq!(
        db.collection("int8")
            .unwrap()
            .get("journal")
            .unwrap()
            .metadata
            .unwrap()["source"],
        "wal"
    );
    assert_eq!(
        db.collection("int8").unwrap().stats().config.quantization,
        stats.config.quantization
    );
    assert_eq!(fs::read(&file.0).unwrap(), snapshot);
    db.checkpoint().unwrap();
    assert_eq!(db.sequence(), 2);
    assert_eq!(fs::metadata(file.wal()).unwrap().len(), 24);
    let reopen = Database::open_read_only(&file.0).unwrap();
    assert_eq!(reopen.collection("int8").unwrap().len(), 3);
}
