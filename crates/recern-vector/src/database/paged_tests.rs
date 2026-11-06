use super::*;
use crate::{CollectionConfig, Metric, Quantization};
use serde_json::json;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("rv-paged-{:032x}", identity()));
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
fn exact_image(db: &Database) -> Vec<u8> {
    storage::encode(&db.collections, db.identity(), db.sequence())
}
fn verify(db: &Database) {
    let reopened = Database::open_read_only(&db.path).unwrap();
    assert_eq!(exact_image(db), exact_image(&reopened));
}
fn seeded(file: &Temp, n: usize) -> Database {
    let mut db = Database::create(&file.0).unwrap();
    db.set_checkpoint_policy(CheckpointPolicy::disabled())
        .unwrap();
    db.create_collection("c", CollectionConfig::new(2, Metric::L2))
        .unwrap()
        .upsert_many_with_threads((0..n).map(|i| (i.to_string(), vec![i as f32, 1.], None)), 1)
        .unwrap();
    db.checkpoint().unwrap();
    db
}
#[test]
fn one_delete_encodes_one_record_with_bounded_validation_at_different_sizes() {
    let mut costs = Vec::new();
    for n in [32, 256, 2048] {
        let file = Temp::new();
        let mut db = seeded(&file, n);
        assert_eq!(db.format_version(), 3);
        let base = fs::read(&file.0).unwrap();
        assert!(db.collection_mut("c").unwrap().delete("12"));
        db.save().unwrap();
        let s = db.persistence_stats();
        assert_eq!(s.last_encoded_records, 1);
        assert_eq!(s.last_written_pages, 2); // header + tombstone, no collection-header change
        assert_eq!(s.last_validated_bytes, 4124);
        costs.push(s.wal_bytes);
        assert_eq!(base, fs::read(&file.0).unwrap());
        verify(&db);
        let seq = db.sequence();
        let log = fs::read(file.wal()).unwrap();
        db.save().unwrap();
        assert_eq!(db.sequence(), seq);
        assert_eq!(db.persistence_stats().last_encoded_records, 0);
        assert_eq!(db.persistence_stats().last_written_pages, 0);
        assert_eq!(log, fs::read(file.wal()).unwrap());
        db.checkpoint().unwrap();
        let after = fs::read(&file.0).unwrap();
        assert_eq!(base.len(), after.len());
        assert_eq!(
            base[..base.len() - 4]
                .as_chunks::<4096>()
                .0
                .iter()
                .zip(after[..after.len() - 4].as_chunks::<4096>().0.iter())
                .filter(|(a, b)| a != b)
                .count(),
            2
        );
    }
    assert!(costs.windows(2).all(|p| p[0] == p[1]), "{costs:?}");
}
#[test]
fn graph_and_vectors_round_trip_exactly_after_sequential_and_parallel_mutations() {
    for q in [Quantization::F32, Quantization::Int8] {
        for metric in [Metric::Cosine, Metric::Dot, Metric::L2] {
            let file = Temp::new();
            let mut db = Database::create(&file.0).unwrap();
            db.set_checkpoint_policy(CheckpointPolicy::disabled())
                .unwrap();
            db.create_collection(
                "東京",
                CollectionConfig::new(3, metric).with_quantization(q),
            )
            .unwrap()
            .upsert(
                "",
                &[1., 2., 3.],
                Some(json!({"large":"x".repeat(12000), "nested":{"国":"JP"}})),
            )
            .unwrap();
            db.create_collection("empty", CollectionConfig::new(3, metric))
                .unwrap();
            db.checkpoint().unwrap();
            verify(&db);
            for (start, count, threads) in [(0, 20, 1), (20, 1100, 4), (1120, 5, 1)] {
                db.collection_mut("東京")
                    .unwrap()
                    .upsert_many_with_threads(
                        (start..start + count).map(|i| {
                            (
                                i.to_string(),
                                vec![i as f32, (i % 13) as f32, 1.],
                                Some(json!({"i": i})),
                            )
                        }),
                        threads,
                    )
                    .unwrap();
                db.save().unwrap();
                verify(&db);
            }
            db.collection_mut("東京")
                .unwrap()
                .upsert("", &[3., 1., 0.], None)
                .unwrap();
            db.collection_mut("東京").unwrap().delete("17");
            db.save().unwrap();
            verify(&db);
            let removed = db.collection_mut("東京").unwrap().compact();
            assert_eq!(removed, 2);
            db.save().unwrap();
            verify(&db);
            db.checkpoint().unwrap();
            verify(&db);
        }
    }
}
#[test]
fn failed_batch_and_absent_delete_do_not_advance_sequence() {
    let file = Temp::new();
    let mut db = seeded(&file, 20);
    let before = exact_image(&db);
    let c = db.collection_mut("c").unwrap();
    assert!(
        c.upsert_many_with_threads([("0", vec![3., 4.], None), ("broken", vec![1.], None)], 1)
            .is_err()
    );
    assert!(!c.delete("absent"));
    db.save().unwrap();
    assert_eq!(before, exact_image(&db));
    assert_eq!(db.persistence_stats().last_written_pages, 0);
    verify(&db);
}
#[test]
fn drop_recreate_compaction_and_free_page_reuse() {
    let file = Temp::new();
    let mut db = seeded(&file, 100);
    let size = fs::metadata(&file.0).unwrap().len();
    for i in 0..5 {
        db.drop_collection("c").unwrap();
        db.save().unwrap();
        verify(&db);
        db.create_collection("c", CollectionConfig::new(2, Metric::Dot))
            .unwrap()
            .upsert_many_with_threads(
                (0..80).map(|j| (format!("{i}:{j}"), vec![j as f32, 1.], None)),
                1,
            )
            .unwrap();
        db.save().unwrap();
        verify(&db);
        db.collection_mut("c").unwrap().delete(&format!("{i}:0"));
        db.collection_mut("c").unwrap().compact();
        db.checkpoint().unwrap();
        verify(&db);
        assert_eq!(fs::metadata(&file.0).unwrap().len(), size);
    }
    // A drop and recreate in one commit reuses the directory safely as well.
    db.drop_collection("c").unwrap();
    db.create_collection("c", CollectionConfig::new(7, Metric::L2))
        .unwrap();
    db.save().unwrap();
    verify(&db);
}
#[test]
fn another_checkpoint_is_allowed_but_equal_sized_replacement_wal_is_stale() {
    let file = Temp::new();
    let mut db = seeded(&file, 100);
    db.collection_mut("c").unwrap().delete("0");
    db.save().unwrap();
    let first_length = fs::metadata(file.wal()).unwrap().len();
    let mut other = Database::open(&file.0).unwrap();
    other.checkpoint().unwrap();
    db.save().unwrap();
    assert_eq!(db.persistence_stats().wal_commits, 0);
    other.collection_mut("c").unwrap().delete("1");
    other.save().unwrap();
    assert_eq!(fs::metadata(file.wal()).unwrap().len(), first_length);
    assert!(
        db.save()
            .unwrap_err()
            .to_string()
            .contains("another handle")
    );
    assert!(
        !Database::open(&file.0)
            .unwrap()
            .collection("c")
            .unwrap()
            .contains("1")
    );
}
#[test]
fn missing_or_truncated_committed_wal_cannot_be_overwritten() {
    for missing in [false, true] {
        let file = Temp::new();
        let mut db = seeded(&file, 8);
        db.collection_mut("c").unwrap().delete("0");
        db.save().unwrap();
        if missing {
            fs::remove_file(file.wal()).unwrap();
        } else {
            fs::write(file.wal(), wal::header(db.identity())).unwrap();
        }
        assert!(db.save().is_err());
    }
}
fn legacy(file: &Temp) -> Database {
    fs::write(
        &file.0,
        include_bytes!("../../tests/fixtures/v2-0.2.0.rvec"),
    )
    .unwrap();
    fs::write(
        file.wal(),
        include_bytes!("../../tests/fixtures/v2-0.2.0.wal"),
    )
    .unwrap();
    Database::open(&file.0).unwrap()
}
#[test]
fn legacy_saves_stay_v2_and_explicit_upgrade_keeps_a_durable_private_backup() {
    let file = Temp::new();
    let mut db = legacy(&file);
    db.create_collection("pending", CollectionConfig::new(2, Metric::L2))
        .unwrap();
    db.save().unwrap();
    assert_eq!(db.format_version(), 2);
    let durable = exact_image(&db);
    let id = db.identity();
    let seq = db.sequence();
    db.collection_mut("pending")
        .unwrap()
        .upsert("pending", &[1., 2.], None)
        .unwrap();
    let backup = db.upgrade_storage().unwrap().unwrap();
    assert_eq!(fs::read(&backup).unwrap(), durable);
    assert_eq!(
        Database::open_read_only(&backup).unwrap().format_version(),
        2
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(db.format_version(), 3);
    assert_eq!(db.identity(), id);
    assert_eq!(db.sequence(), seq + 1);
    assert_eq!(db.persistence_stats().wal_bytes, 24);
    verify(&db);
    assert!(db.upgrade_storage().unwrap().is_none());
    assert!(
        Database::open_read_only(&file.0)
            .unwrap()
            .upgrade_storage()
            .is_err()
    );
}
#[test]
fn v1_requires_explicit_checkpoint_before_upgrade() {
    let file = Temp::new();
    fs::write(
        &file.0,
        include_bytes!("../../tests/fixtures/v1-0.0.2.rvec"),
    )
    .unwrap();
    let db = Database::open(&file.0).unwrap();
    assert!(db.upgrade_storage().is_err());
    db.checkpoint().unwrap();
    assert_eq!(db.format_version(), 2);
    db.upgrade_storage().unwrap();
    assert_eq!(db.format_version(), 3);
    verify(&db);
}
#[test]
fn upgrade_interruption_keeps_a_complete_old_or_new_snapshot() {
    for phase in [
        CheckpointBoundary::SnapshotWritten,
        CheckpointBoundary::SnapshotSynced,
        CheckpointBoundary::SnapshotPublished,
        CheckpointBoundary::SnapshotRenamed,
        CheckpointBoundary::WalReset,
    ] {
        let file = Temp::new();
        let db = legacy(&file);
        let before = exact_image(&db);
        *db.checkpoint_fault.lock().unwrap() = Some(super::tests::Fault { phase, exit: false });
        assert!(db.upgrade_storage().is_err());
        let reopened = Database::open(&file.0).unwrap();
        assert_eq!(
            storage::encode(&db.collections, db.identity(), 0),
            storage::encode(&reopened.collections, reopened.identity(), 0)
        );
        assert_eq!(
            reopened.format_version(),
            if matches!(
                phase,
                CheckpointBoundary::SnapshotWritten | CheckpointBoundary::SnapshotSynced
            ) {
                2
            } else {
                3
            }
        );
        assert!(
            fs::read_dir(file.0.parent().unwrap())
                .unwrap()
                .filter_map(|e| e.ok())
                .any(|e| e.path().extension().is_some_and(|s| s == "bak")
                    && fs::read(e.path()).unwrap() == before)
        );
        if phase == CheckpointBoundary::SnapshotPublished {
            // Rename succeeded, but its durability barrier failed before the
            // handle could adopt the new format. Reopen after this ambiguous error.
            assert!(db.upgrade_storage().is_err());
            reopened.upgrade_storage().unwrap();
            reopened.checkpoint().unwrap();
            verify(&reopened);
        } else {
            db.upgrade_storage().unwrap();
            verify(&db);
        }
    }
}

#[test]
fn process_exit_during_explicit_upgrade_preserves_backup_and_recovery() {
    for (phase, format) in [
        ("written", 2),
        ("synced", 2),
        ("published", 3),
        ("renamed", 3),
        ("reset", 3),
    ] {
        let file = Temp::new();
        let db = legacy(&file);
        let before = exact_image(&db);
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "database::tests::crash_worker", "--nocapture"])
            .env("RECERN_CHECKPOINT_TEST_PATH", &file.0)
            .env("RECERN_CHECKPOINT_TEST_PHASE", phase)
            .env("RECERN_CHECKPOINT_TEST_UPGRADE", "1")
            .status()
            .unwrap();
        assert_eq!(result.code(), Some(73));
        let recovered = Database::open(&file.0).unwrap();
        assert_eq!(recovered.format_version(), format);
        assert_eq!(
            storage::encode(&db.collections, db.identity(), 0),
            storage::encode(&recovered.collections, recovered.identity(), 0)
        );
        assert!(
            fs::read_dir(file.0.parent().unwrap())
                .unwrap()
                .filter_map(|e| e.ok())
                .any(|e| e.path().extension().is_some_and(|s| s == "bak")
                    && fs::read(e.path()).unwrap() == before)
        );
        recovered.upgrade_storage().unwrap();
        recovered.checkpoint().unwrap();
        verify(&recovered);
    }
}

#[cfg(unix)]
#[test]
fn new_data_files_are_private_and_checkpoints_upgrades_preserve_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let file = Temp::new();
    let mut db = seeded(&file, 3);
    for p in [&file.0, file.wal().as_path()] {
        assert_eq!(fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o600);
    }
    fs::set_permissions(&file.0, fs::Permissions::from_mode(0o640)).unwrap();
    db.collection_mut("c").unwrap().delete("0");
    db.checkpoint().unwrap();
    assert_eq!(
        fs::metadata(&file.0).unwrap().permissions().mode() & 0o777,
        0o640
    );
    let old = Temp::new();
    let db = legacy(&old);
    fs::set_permissions(&old.0, fs::Permissions::from_mode(0o600)).unwrap();
    db.upgrade_storage().unwrap();
    assert_eq!(
        fs::metadata(&old.0).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn torn_second_commit_keeps_the_last_acknowledgement_in_both_formats() {
    for format in [2, 3] {
        let file = Temp::new();
        let db = seeded(&file, 1);
        if format == 2 {
            let bytes = exact_image(&db);
            drop(db);
            fs::write(&file.0, bytes).unwrap();
        } else {
            drop(db);
        }
        let mut db = Database::open(&file.0).unwrap();
        db.set_checkpoint_policy(CheckpointPolicy::disabled())
            .unwrap();
        db.collection_mut("c")
            .unwrap()
            .upsert("acknowledged", &[2., 3.], None)
            .unwrap();
        db.save().unwrap();
        let base = fs::read(&file.0).unwrap();
        let acknowledged = fs::read(file.wal()).unwrap();
        let sequence = db.sequence();
        db.collection_mut("c")
            .unwrap()
            .upsert("incomplete", &[3., 4.], None)
            .unwrap();
        db.save().unwrap();
        let full = fs::read(file.wal()).unwrap();
        drop(db);
        let mut cuts: Vec<_> = (acknowledged.len()..full.len()).step_by(512).collect();
        cuts.extend(acknowledged.len()..(acknowledged.len() + 16).min(full.len()));
        cuts.extend(full.len().saturating_sub(16)..full.len());
        cuts.sort_unstable();
        cuts.dedup();
        for cut in cuts {
            fs::write(&file.0, &base).unwrap();
            fs::write(file.wal(), &full[..cut]).unwrap();
            let mut recovered = Database::open(&file.0).unwrap();
            assert_eq!(recovered.format_version(), format);
            assert_eq!(recovered.sequence(), sequence, "format {format}, cut {cut}");
            let col = recovered.collection_mut("c").unwrap();
            assert!(col.contains("acknowledged"));
            assert!(!col.contains("incomplete"));
            col.upsert("continued", &[4., 5.], None).unwrap();
            recovered.save().unwrap();
            assert_eq!(
                Database::open_read_only(&file.0)
                    .unwrap()
                    .collection("c")
                    .unwrap()
                    .len(),
                3
            );
        }
        // Restore a matched base+WAL pair at another path, then make it standalone.
        let restored = Temp::new();
        fs::write(&restored.0, &base).unwrap();
        fs::write(restored.wal(), &acknowledged).unwrap();
        let recovered = Database::open(&restored.0).unwrap();
        assert_eq!(recovered.sequence(), sequence);
        recovered.checkpoint().unwrap();
        fs::remove_file(restored.wal()).unwrap();
        assert_eq!(
            Database::open_read_only(&restored.0)
                .unwrap()
                .collection("c")
                .unwrap()
                .len(),
            2
        );
        // A WAL belonging to a different database must never be applied.
        let foreign = Temp::new();
        let other = seeded(&foreign, 1);
        drop(other);
        fs::write(foreign.wal(), acknowledged).unwrap();
        assert!(matches!(Database::open(&foreign.0), Err(Error::Corrupt(_))));
    }
}
