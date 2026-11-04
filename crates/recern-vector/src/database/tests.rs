use super::*;
use crate::{CollectionConfig, Metric, Quantization, SearchOptions};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(super) struct Fault {
    pub(super) phase: CheckpointBoundary,
    pub(super) exit: bool,
}
pub(super) fn fault(db: &Database, phase: CheckpointBoundary) -> Result<()> {
    let mut fault = db.checkpoint_fault.lock().unwrap();
    if fault.as_ref().is_some_and(|f| f.phase == phase) {
        if fault.take().unwrap().exit {
            std::process::exit(73);
        }
        return Err(std::io::Error::other("injected checkpoint failure").into());
    }
    Ok(())
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("rv-checkpoint-{:032x}", identity()));
        fs::create_dir(&dir).unwrap();
        Self(dir.join("test.rvec"))
    }
    fn wal(&self) -> PathBuf {
        wal::sidecar(&self.0, ".wal")
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().unwrap());
    }
}
fn policy(commits: u64) -> CheckpointPolicy {
    CheckpointPolicy {
        max_wal_bytes: None,
        max_commits: Some(commits),
    }
}
fn seeded(path: &Path, quantization: Quantization) -> Database {
    let mut db = Database::create(path).unwrap();
    db.create_collection(
        "docs",
        CollectionConfig::new(2, Metric::L2).with_quantization(quantization),
    )
    .unwrap()
    .upsert("first", &[1., 2.], None)
    .unwrap();
    db.checkpoint().unwrap();
    db
}
fn append(db: &mut Database, id: &str) {
    db.collection_mut("docs")
        .unwrap()
        .upsert(id, &[3., 4.], None)
        .unwrap();
}

#[test]
fn thresholds_survive_reopen_as_wal_counts_but_policy_is_per_handle() {
    for quantization in [Quantization::F32, Quantization::Int8] {
        let file = Temp::new();
        let mut db = seeded(&file.0, quantization);
        db.set_checkpoint_policy(policy(2)).unwrap();
        append(&mut db, "second");
        db.save().unwrap();
        assert_eq!(db.persistence_stats().wal_commits, 1);
        assert_eq!(db.persistence_stats().automatic_checkpoints, 0);
        drop(db);
        let mut db = Database::open(&file.0).unwrap();
        assert_eq!(db.checkpoint_policy(), CheckpointPolicy::default());
        assert_eq!(db.persistence_stats().wal_commits, 1);
        db.set_checkpoint_policy(policy(2)).unwrap();
        append(&mut db, "third");
        db.save().unwrap();
        let stats = db.persistence_stats();
        assert_eq!(
            (
                stats.wal_bytes,
                stats.wal_commits,
                stats.automatic_checkpoints
            ),
            (24, 0, 1)
        );
        assert_eq!(db.format_version(), 3);
        assert_eq!(db.sequence(), 3);
        assert_eq!(
            Database::open_read_only(&file.0)
                .unwrap()
                .collection("docs")
                .unwrap()
                .len(),
            3
        );
    }
}
#[test]
fn byte_limit_checkpoints_one_large_commit_and_disable_keeps_manual_control() {
    let file = Temp::new();
    let mut db = seeded(&file.0, Quantization::F32);
    db.set_checkpoint_policy(CheckpointPolicy {
        max_wal_bytes: Some(1),
        max_commits: None,
    })
    .unwrap();
    append(&mut db, "second");
    db.save().unwrap();
    assert_eq!(db.persistence_stats().automatic_checkpoints, 1);
    assert_eq!(fs::metadata(file.wal()).unwrap().len(), 24);
    let standalone = Temp::new();
    fs::copy(&file.0, &standalone.0).unwrap();
    assert_eq!(
        Database::open_read_only(&standalone.0)
            .unwrap()
            .collection("docs")
            .unwrap()
            .len(),
        2
    );
    db.set_checkpoint_policy(CheckpointPolicy::disabled())
        .unwrap();
    let base = fs::read(&file.0).unwrap();
    for id in ["third", "fourth", "fifth"] {
        append(&mut db, id);
        db.save().unwrap();
    }
    assert_eq!(fs::read(&file.0).unwrap(), base);
    assert_eq!(db.persistence_stats().wal_commits, 3);
    db.checkpoint().unwrap();
    assert_eq!(db.persistence_stats().wal_commits, 0);
    assert_eq!(db.persistence_stats().automatic_checkpoints, 1);
}
#[test]
fn no_op_save_can_service_due_maintenance_without_a_new_commit() {
    let file = Temp::new();
    let mut db = seeded(&file.0, Quantization::F32);
    db.set_checkpoint_policy(CheckpointPolicy::disabled())
        .unwrap();
    append(&mut db, "second");
    db.save().unwrap();
    let sequence = db.sequence();
    db.set_checkpoint_policy(policy(1)).unwrap();
    db.save().unwrap();
    assert_eq!(db.sequence(), sequence);
    assert_eq!(db.persistence_stats().automatic_checkpoints, 1);
    let base = fs::read(&file.0).unwrap();
    let wal = fs::read(file.wal()).unwrap();
    db.save().unwrap();
    assert_eq!(db.sequence(), sequence);
    assert_eq!(db.persistence_stats().automatic_checkpoints, 1);
    assert_eq!(fs::read(&file.0).unwrap(), base);
    assert_eq!(fs::read(file.wal()).unwrap(), wal);
}
#[test]
fn invalid_policy_is_rejected_without_changing_it_and_read_only_never_maintains() {
    let file = Temp::new();
    let mut db = seeded(&file.0, Quantization::F32);
    for invalid in [
        CheckpointPolicy {
            max_wal_bytes: Some(0),
            max_commits: None,
        },
        policy(0),
    ] {
        assert!(db.set_checkpoint_policy(invalid).is_err());
        assert_eq!(db.checkpoint_policy(), CheckpointPolicy::default());
    }
    append(&mut db, "second");
    db.save().unwrap();
    let base = fs::read(&file.0).unwrap();
    let wal = fs::read(file.wal()).unwrap();
    let mut ro = Database::open_read_only(&file.0).unwrap();
    assert_eq!(ro.persistence_stats().wal_commits, 1);
    assert!(ro.set_checkpoint_policy(policy(1)).is_err());
    assert!(ro.save().is_err());
    assert!(ro.checkpoint().is_err());
    assert_eq!(fs::read(&file.0).unwrap(), base);
    assert_eq!(fs::read(file.wal()).unwrap(), wal);
}
#[test]
fn maintenance_failure_keeps_the_acknowledged_commit_and_retries_without_mutation() {
    for phase in [
        CheckpointBoundary::SnapshotWritten,
        CheckpointBoundary::SnapshotSynced,
        CheckpointBoundary::SnapshotRenamed,
    ] {
        let file = Temp::new();
        let mut db = seeded(&file.0, Quantization::F32);
        db.set_checkpoint_policy(policy(1)).unwrap();
        *db.checkpoint_fault.lock().unwrap() = Some(Fault { phase, exit: false });
        append(&mut db, "durable");
        db.save().unwrap();
        let sequence = db.sequence();
        assert_eq!(db.persistence_stats().automatic_checkpoints, 0);
        assert!(
            db.persistence_stats()
                .last_checkpoint_error
                .unwrap()
                .contains("injected checkpoint failure")
        );
        let recovered = Database::open_read_only(&file.0).unwrap();
        assert!(recovered.collection("docs").unwrap().contains("durable"));
        assert_eq!(recovered.sequence(), sequence);
        db.save().unwrap();
        assert_eq!(db.sequence(), sequence);
        assert_eq!(db.persistence_stats().automatic_checkpoints, 1);
        assert_eq!(db.persistence_stats().wal_bytes, 24);
        assert!(db.persistence_stats().last_checkpoint_error.is_none());
    }
}
#[test]
fn torn_tail_is_not_counted_and_new_commit_checkpoints_only_committed_records() {
    let file = Temp::new();
    let mut db = seeded(&file.0, Quantization::F32);
    db.set_checkpoint_policy(CheckpointPolicy::disabled())
        .unwrap();
    append(&mut db, "second");
    db.save().unwrap();
    let first = fs::read(file.wal()).unwrap();
    append(&mut db, "lost");
    db.save().unwrap();
    let mut log = fs::read(file.wal()).unwrap();
    log.pop();
    fs::write(file.wal(), log).unwrap();
    let mut recovered = Database::open(&file.0).unwrap();
    assert_eq!(recovered.persistence_stats().wal_bytes, first.len() as u64);
    assert_eq!(recovered.persistence_stats().wal_commits, 1);
    recovered.set_checkpoint_policy(policy(2)).unwrap();
    append(&mut recovered, "after-crash");
    recovered.save().unwrap();
    assert_eq!(recovered.persistence_stats().automatic_checkpoints, 1);
    let reopened = Database::open(&file.0).unwrap();
    let c = reopened.collection("docs").unwrap();
    assert_eq!(c.len(), 3);
    assert!(!c.contains("lost"));
    assert!(c.contains("after-crash"));
    assert!(db.save().is_err());
}
#[test]
fn concurrent_saves_checkpoint_once() {
    let file = Temp::new();
    let mut db = seeded(&file.0, Quantization::F32);
    db.set_checkpoint_policy(policy(1)).unwrap();
    append(&mut db, "second");
    let db = Arc::new(db);
    std::thread::scope(|s| {
        for _ in 0..8 {
            let db = db.clone();
            s.spawn(move || db.save().unwrap());
        }
    });
    assert_eq!(db.persistence_stats().automatic_checkpoints, 1);
    assert_eq!(db.sequence(), 2);
    assert_eq!(
        Database::open_read_only(&file.0)
            .unwrap()
            .collection("docs")
            .unwrap()
            .len(),
        2
    );
}
#[test]
fn crash_worker() {
    let Some(path) = std::env::var_os("RECERN_CHECKPOINT_TEST_PATH") else {
        return;
    };
    let phase = match std::env::var("RECERN_CHECKPOINT_TEST_PHASE")
        .unwrap()
        .as_str()
    {
        "wal-written" => CheckpointBoundary::WalWritten,
        "wal-synced" => CheckpointBoundary::WalSynced,
        "written" => CheckpointBoundary::SnapshotWritten,
        "synced" => CheckpointBoundary::SnapshotSynced,
        "renamed" => CheckpointBoundary::SnapshotRenamed,
        "reset" => CheckpointBoundary::WalReset,
        _ => panic!("invalid fixture phase"),
    };
    let mut db = Database::open(path).unwrap();
    db.set_checkpoint_policy(policy(1)).unwrap();
    *db.checkpoint_fault.lock().unwrap() = Some(Fault { phase, exit: true });
    if std::env::var_os("RECERN_CHECKPOINT_TEST_UPGRADE").is_some() {
        db.upgrade_storage().unwrap();
        panic!("upgrade crash injection was not reached");
    }
    append(&mut db, "acknowledged-in-wal");
    db.save().unwrap();
    panic!("crash injection was not reached");
}
#[test]
fn process_exit_at_each_checkpoint_boundary_recovers_the_complete_wal_commit() {
    for phase in [
        "wal-written",
        "wal-synced",
        "written",
        "synced",
        "renamed",
        "reset",
    ] {
        let file = Temp::new();
        let stale = seeded(&file.0, Quantization::Int8);
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "database::tests::crash_worker", "--nocapture"])
            .env("RECERN_CHECKPOINT_TEST_PATH", &file.0)
            .env("RECERN_CHECKPOINT_TEST_PHASE", phase)
            .status()
            .unwrap();
        assert_eq!(result.code(), Some(73), "{phase}");
        let mut db = Database::open(&file.0).unwrap();
        let c = db.collection("docs").unwrap();
        assert_eq!(c.len(), 2, "{phase}");
        assert_eq!(
            c.search(&[3., 4.], 1, &SearchOptions::default().exact())
                .unwrap()[0]
                .id,
            "acknowledged-in-wal"
        );
        assert_eq!(db.sequence(), 2, "{phase}");
        assert!(stale.save().is_err());
        db.set_checkpoint_policy(policy(1)).unwrap();
        append(&mut db, "continued");
        db.save().unwrap();
        assert_eq!(
            Database::open_read_only(&file.0)
                .unwrap()
                .collection("docs")
                .unwrap()
                .len(),
            3,
            "{phase}"
        );
    }
}
