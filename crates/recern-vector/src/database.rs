use crate::collection::{Collection, CollectionConfig};
use crate::error::{Error, Result};
use crate::{paged, storage, wal};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// An in-memory database backed by a snapshot and a checksummed `.wal` journal.
/// Mutations become durable at `save()`. Use `checkpoint()` before copying a
/// standalone file. Concurrent handles can read; stale writers fail on save.
pub struct Database {
    path: PathBuf,
    collections: BTreeMap<String, Collection>,
    persistence: Mutex<Persistence>,
    #[cfg(test)]
    checkpoint_fault: Mutex<Option<tests::Fault>>,
    read_only: bool,
}
/// Synchronous maintenance policy checked after a durable save, or on a no-op
/// save when a checkpoint is due. Limits are triggers, not a hard disk quota.
/// Policy belongs to this handle and is not stored in the database file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckpointPolicy {
    /// Valid WAL bytes including its header; `None` disables this trigger.
    pub max_wal_bytes: Option<u64>,
    /// Commits not yet included in the base snapshot; `None` disables it.
    pub max_commits: Option<u64>,
}
impl Default for CheckpointPolicy {
    fn default() -> Self {
        Self {
            max_wal_bytes: Some(64 * 1024 * 1024),
            max_commits: Some(1_000),
        }
    }
}
impl CheckpointPolicy {
    pub fn new(max_wal_bytes: Option<u64>, max_commits: Option<u64>) -> Result<Self> {
        let policy = Self {
            max_wal_bytes,
            max_commits,
        };
        policy.validate()?;
        Ok(policy)
    }
    pub const fn disabled() -> Self {
        Self {
            max_wal_bytes: None,
            max_commits: None,
        }
    }
    fn validate(self) -> Result<()> {
        if self.max_wal_bytes == Some(0) || self.max_commits == Some(0) {
            return Err(Error::InvalidArgument(
                "checkpoint thresholds must be positive or disabled".into(),
            ));
        }
        Ok(())
    }
    fn due(self, state: &Persistence) -> bool {
        (self.max_wal_bytes.is_some() || self.max_commits.is_some())
            && state.wal_bytes > wal::HEADER as u64
            && (state.last_checkpoint_error.is_some()
                || self.max_wal_bytes.is_some_and(|n| state.wal_bytes >= n)
                || self.max_commits.is_some_and(|n| state.wal_commits >= n))
    }
}

/// Persistence state of this handle at its last save/open, not a live view of
/// other writers. WAL bytes exclude incomplete tails but include retained obsolete
/// frames. Commit counts exclude frames already included in the base snapshot. Automatic checkpoint errors do not undo a durable WAL
/// commit; callers can inspect them here and retry maintenance with `save()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistenceStats {
    pub wal_bytes: u64,
    pub wal_commits: u64,
    pub automatic_checkpoints: u64,
    pub last_checkpoint_error: Option<String>,
    /// Records serialized by the last save; zero on a clean format-3 no-op.
    pub last_encoded_records: u64,
    /// Format-3 pages changed by the last save (excluding the root footer).
    pub last_written_pages: u64,
    /// Bytes read to validate the last format-3 writer position.
    pub last_validated_bytes: u64,
}
struct Persistence {
    needs_sync: bool,
    bytes: Vec<u8>,
    paged: Option<paged::Image>,
    base: wal::Stamp,
    last_encoded_records: u64,
    last_written_pages: u64,
    last_validated_bytes: u64,
    policy: CheckpointPolicy,
    wal_bytes: u64,
    wal_commits: u64,
    automatic_checkpoints: u64,
    last_checkpoint_error: Option<String>,
}
impl Persistence {
    fn new(bytes: Vec<u8>, wal_bytes: u64, wal_commits: u64) -> Result<Self> {
        let base = wal::Stamp::from_bytes(&bytes);
        let paged = if base.version == 3 {
            Some(paged::Image::from_bytes(&bytes)?)
        } else {
            None
        };
        let bytes = if paged.is_some() { Vec::new() } else { bytes };
        Ok(Self {
            needs_sync: false,
            bytes,
            paged,
            base,
            last_encoded_records: 0,
            last_written_pages: 0,
            last_validated_bytes: 0,
            policy: CheckpointPolicy::default(),
            wal_bytes,
            wal_commits,
            automatic_checkpoints: 0,
            last_checkpoint_error: None,
        })
    }
    fn stats(&self) -> PersistenceStats {
        PersistenceStats {
            wal_bytes: self.wal_bytes,
            wal_commits: self.wal_commits,
            automatic_checkpoints: self.automatic_checkpoints,
            last_checkpoint_error: self.last_checkpoint_error.clone(),
            last_encoded_records: self.last_encoded_records,
            last_written_pages: self.last_written_pages,
            last_validated_bytes: self.last_validated_bytes,
        }
    }
}

struct Lock(File);
impl Lock {
    fn write(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(wal::sidecar(path, ".lock"))?;
        file.lock()?;
        Ok(Self(file))
    }
    fn read(path: &Path) -> Result<Option<Self>> {
        match File::open(wal::sidecar(path, ".lock")) {
            Ok(file) => {
                file.lock_shared()?;
                Ok(Some(Self(file)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl Database {
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let path = absolute(path.as_ref())?;
        let _lock = Lock::write(&path)?;
        if path.exists() {
            return Err(Error::FileExists(path));
        }
        if wal::sidecar(&path, ".wal").exists() {
            return Err(Error::InvalidArgument(
                "orphan WAL exists; recover or remove it before creating a database".into(),
            ));
        }
        let collections = BTreeMap::new();
        let bytes = paged::Image::empty(identity()).bytes();
        atomic_snapshot(&path, &bytes)?;
        Ok(Self {
            path,
            collections,
            persistence: Mutex::new(Persistence::new(bytes, 0, 0)?),
            #[cfg(test)]
            checkpoint_fault: Mutex::new(None),
            read_only: false,
        })
    }
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::load(path.as_ref(), false)
    }
    /// Loads a consistent snapshot including committed WAL records, without
    /// creating sidecars or allowing mutations through this handle.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        Self::load(path.as_ref(), true)
    }
    fn load(path: &Path, read_only: bool) -> Result<Self> {
        let path = fs::canonicalize(path)?;
        let _lock = if read_only {
            Lock::read(&path)?
        } else {
            Some(Lock::write(&path)?)
        };
        let loaded = wal::load(&path)?;
        let collections = storage::decode(&loaded.bytes)?;
        let mut persistence = Persistence::new(loaded.bytes, loaded.wal_len, loaded.commits)?;
        // A complete frame can be visible after a writer's failed fsync. Reads
        // may recover it, but the next successful save must establish durability
        // even when it has no logical changes (e.g. an idempotent retry).
        persistence.needs_sync = true;
        persistence.base = loaded.base;
        if let Some(image) = &mut persistence.paged {
            image.bind(&collections);
        }
        Ok(Self {
            path,
            collections,
            persistence: Mutex::new(persistence),
            #[cfg(test)]
            checkpoint_fault: Mutex::new(None),
            read_only,
        })
    }
    pub fn open_or_create(path: impl AsRef<Path>) -> Result<Self> {
        match Self::open(&path) {
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                match Self::create(&path) {
                    Err(Error::FileExists(_)) => Self::open(path),
                    result => result,
                }
            }
            result => result,
        }
    }
    /// Format of this handle's last durable snapshot (including WAL).
    pub fn format_version(&self) -> u32 {
        let bytes = self
            .persistence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if bytes.paged.is_some() {
            3
        } else {
            u32::from_le_bytes(bytes.bytes[4..8].try_into().expect("validated header"))
        }
    }
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Persistent format-2/3 database identity. Legacy format-1 files return
    /// zero until their first save upgrades them; checkpoints keep the ID.
    pub fn identity(&self) -> u128 {
        let bytes = self
            .persistence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(image) = &bytes.paged {
            image.id
        } else {
            storage::header(&bytes.bytes).0
        }
    }

    /// Last durable commit sequence, including committed WAL frames.
    pub fn sequence(&self) -> u64 {
        let bytes = self
            .persistence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(image) = &bytes.paged {
            image.sequence
        } else {
            storage::header(&bytes.bytes).1
        }
    }
    fn writable(&self) -> Result<()> {
        if self.read_only {
            Err(Error::InvalidArgument("database is read-only".into()))
        } else {
            Ok(())
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn create_collection(
        &mut self,
        name: &str,
        config: CollectionConfig,
    ) -> Result<&mut Collection> {
        self.writable()?;
        if name.is_empty() || name.len() > 255 {
            return Err(Error::InvalidArgument(
                "collection name must be 1-255 bytes".into(),
            ));
        }
        if self.collections.contains_key(name) {
            return Err(Error::CollectionExists(name.to_owned()));
        }
        let collection = Collection::new(name, config)?;
        Ok(self
            .collections
            .entry(name.to_owned())
            .or_insert(collection))
    }

    pub fn collection(&self, name: &str) -> Result<&Collection> {
        self.collections
            .get(name)
            .ok_or_else(|| Error::CollectionNotFound(name.to_owned()))
    }

    pub fn collection_mut(&mut self, name: &str) -> Result<&mut Collection> {
        self.writable()?;
        self.collections
            .get_mut(name)
            .ok_or_else(|| Error::CollectionNotFound(name.to_owned()))
    }

    pub fn drop_collection(&mut self, name: &str) -> Result<()> {
        self.writable()?;
        self.collections
            .remove(name)
            .map(|_| ())
            .ok_or_else(|| Error::CollectionNotFound(name.to_owned()))
    }

    pub fn collections(&self) -> impl Iterator<Item = &Collection> {
        self.collections.values()
    }

    /// Policy for this handle. Reopening uses the default policy again.
    pub fn checkpoint_policy(&self) -> CheckpointPolicy {
        self.persistence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .policy
    }
    pub fn set_checkpoint_policy(&mut self, policy: CheckpointPolicy) -> Result<()> {
        self.writable()?;
        policy.validate()?;
        self.persistence
            .get_mut()
            .map_err(|_| Error::InvalidArgument("persistence lock poisoned".into()))?
            .policy = policy;
        Ok(())
    }
    pub fn persistence_stats(&self) -> PersistenceStats {
        self.persistence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stats()
    }

    /// Appends changed pages and fsyncs a commit. A due checkpoint is attempted
    /// synchronously after that durability barrier. Maintenance failure leaves
    /// the commit successful and is exposed in `persistence_stats()`; a later
    /// save retries it without appending another frame for unchanged data.
    pub fn save(&self) -> Result<()> {
        self.persist(false)
    }
    /// Atomically merges committed and pending changes into the base snapshot,
    /// then resets WAL. A crash between those steps is safe to recover.
    pub fn checkpoint(&self) -> Result<()> {
        self.persist(true)
    }
    fn persist(&self, checkpoint: bool) -> Result<()> {
        self.writable()?;
        let mut state = self
            .persistence
            .lock()
            .map_err(|_| Error::InvalidArgument("persistence lock poisoned".into()))?;
        let _lock = Lock::write(&self.path)?;
        if state.paged.is_some() {
            return self.persist_paged(&mut state, checkpoint);
        }
        state.last_encoded_records = self.collections.values().map(|c| c.ids.len() as u64).sum();
        state.last_written_pages = 0;
        state.last_validated_bytes = 0;
        let disk = wal::load(&self.path)?;
        if disk.bytes != state.bytes {
            return Err(Error::InvalidArgument(
                "database changed in another handle; reopen before writing".into(),
            ));
        }
        state.wal_bytes = disk.wal_len;
        state.wal_commits = disk.commits;
        self.sync_recovered(&mut state)?;
        let (mut id, seq) = storage::header(&state.bytes);
        let migrate = id == 0;
        if migrate {
            id = identity();
        }
        let mut bytes = storage::encode(&self.collections, id, seq);
        let unchanged = bytes == state.bytes;
        if !unchanged {
            let next = seq
                .checked_add(1)
                .ok_or_else(|| Error::InvalidArgument("commit sequence exhausted".into()))?;
            // Reuse the encoded image instead of serializing every collection
            // a second time just to advance its header sequence.
            storage::set_sequence(&mut bytes, next);
        }
        if checkpoint || migrate {
            self.write_checkpoint(&mut state, Some(bytes))?;
            state.last_checkpoint_error = None;
        } else {
            if !unchanged {
                let next = storage::header(&bytes).1;
                let frame = wal::frame(&state.bytes, &bytes, next);
                wal::append(&self.path, disk.wal_len, id, &frame)?;
                state.wal_bytes = disk.wal_len.max(wal::HEADER as u64) + frame.len() as u64;
                state.wal_commits += 1;
                state.bytes = bytes;
            }
            if state.policy.due(&state) {
                // WAL is already synced. Never turn a maintenance failure into
                // a failed mutation acknowledgement that invites a blind retry.
                match self.write_checkpoint(&mut state, None) {
                    Ok(()) => {
                        state.automatic_checkpoints = state.automatic_checkpoints.saturating_add(1);
                        state.last_checkpoint_error = None;
                    }
                    Err(error) => state.last_checkpoint_error = Some(error.to_string()),
                }
            }
        }
        Ok(())
    }
    fn persist_paged(&self, state: &mut Persistence, checkpoint: bool) -> Result<()> {
        let image = state.paged.as_ref().unwrap();
        let current = wal::Stamp {
            version: 3,
            id: image.id,
            sequence: image.sequence,
            checksum: image.root(),
            len: image.len() as u64,
        };
        let disk = wal::current(
            &self.path,
            state.base,
            current,
            state.wal_bytes,
            state.wal_commits,
        )?;
        state.base = disk.base;
        state.wal_bytes = disk.wal_len;
        state.wal_commits = disk.commits;
        state.last_validated_bytes = disk.read_bytes;
        let patch = image.plan(&self.collections)?;
        state.last_encoded_records = patch.encoded;
        state.last_written_pages = patch.pages.len() as u64;
        let image_id = image.id;
        self.sync_recovered(state)?;
        if !patch.pages.is_empty() {
            let pages = patch
                .pages
                .iter()
                .map(|(&i, b)| ((i * paged::PAGE) as u64, b.to_vec()))
                .chain(std::iter::once((
                    (patch.count * paged::PAGE) as u64,
                    patch.root.to_le_bytes().to_vec(),
                )));
            let frame = wal::frame_pages(
                current.checksum,
                (patch.count * paged::PAGE + 4) as u64,
                patch.sequence,
                pages,
            );
            let wal_bytes = disk
                .wal_len
                .max(wal::HEADER as u64)
                .checked_add(frame.len() as u64)
                .ok_or_else(|| Error::InvalidArgument("WAL size exhausted".into()))?;
            wal::append_with(&self.path, disk.wal_len, image_id, &frame, || {
                self.checkpoint_boundary(CheckpointBoundary::WalWritten)
            })?;
            self.checkpoint_boundary(CheckpointBoundary::WalSynced)?;
            state.wal_bytes = wal_bytes;
            state.wal_commits += 1;
        }
        state.paged.as_mut().unwrap().apply(patch);
        for c in self.collections.values() {
            c.changes.saved();
            c.graph.changes.saved();
        }
        if checkpoint {
            self.write_paged_checkpoint(state)?;
            state.last_checkpoint_error = None;
        } else if state.policy.due(state) {
            match self.write_paged_checkpoint(state) {
                Ok(()) => {
                    state.automatic_checkpoints = state.automatic_checkpoints.saturating_add(1);
                    state.last_checkpoint_error = None;
                }
                Err(e) => state.last_checkpoint_error = Some(e.to_string()),
            }
        }
        Ok(())
    }
    fn sync_recovered(&self, state: &mut Persistence) -> Result<()> {
        if !state.needs_sync {
            return Ok(());
        }
        self.checkpoint_boundary(CheckpointBoundary::RecoverySync)?;
        let sync = |path: &Path| -> std::io::Result<()> {
            OpenOptions::new()
                .read(true)
                .write(cfg!(windows))
                .open(path)?
                .sync_all()
        };
        sync(&self.path)?;
        match sync(&wal::sidecar(&self.path, ".wal")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && state.wal_bytes == 0 => {}
            result => result?,
        }
        sync_parent_dir(&self.path)?;
        state.needs_sync = false;
        Ok(())
    }
    fn write_paged_checkpoint(&self, state: &mut Persistence) -> Result<()> {
        let image = state.paged.as_ref().unwrap();
        atomic_snapshot_stream(
            &self.path,
            |file| image.write(file),
            |phase| self.checkpoint_boundary(phase),
        )?;
        state.base = wal::Stamp {
            version: 3,
            id: image.id,
            sequence: image.sequence,
            checksum: image.root(),
            len: image.len() as u64,
        };
        state.wal_commits = 0;
        self.checkpoint_boundary(CheckpointBoundary::SnapshotRenamed)?;
        atomic_snapshot(&wal::sidecar(&self.path, ".wal"), &wal::header(image.id))?;
        state.wal_bytes = wal::HEADER as u64;
        self.checkpoint_boundary(CheckpointBoundary::WalReset)?;
        Ok(())
    }
    /// Explicitly convert format 2 to paged format 3. Returns a private,
    /// standalone backup of the last durable format-2 state. Pending changes
    /// are included in the new snapshot; an error after rename may have committed
    /// the upgrade. Reopen to inspect. Already-paged handles return `None`.
    /// Format-1 callers must checkpoint to format 2 first. Stop other writers
    /// during upgrades/backups. Old readers reject format 3 explicitly.
    pub fn upgrade_storage(&self) -> Result<Option<PathBuf>> {
        self.writable()?;
        let mut state = self
            .persistence
            .lock()
            .map_err(|_| Error::InvalidArgument("persistence lock poisoned".into()))?;
        let _lock = Lock::write(&self.path)?;
        if state.paged.is_some() {
            return Ok(None);
        }
        let disk = wal::load(&self.path)?;
        if disk.bytes != state.bytes {
            return Err(Error::InvalidArgument(
                "database changed in another handle; reopen before writing".into(),
            ));
        }
        if disk.base.version == 1 {
            return Err(Error::InvalidArgument(
                "checkpoint format-1 data to format 2 before upgrading storage".into(),
            ));
        }
        let (id, seq) = storage::header(&state.bytes);
        let mut image = paged::Image::empty_at(id, seq);
        let patch = image.plan(&self.collections)?;
        image.apply(patch);
        let backup = wal::sidecar(&self.path, &format!(".format-2-{:032x}.bak", identity()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&backup)?;
        file.write_all(&disk.bytes)?;
        file.sync_all()?;
        sync_parent_dir(&backup)?;
        drop(file);
        atomic_snapshot_stream(
            &self.path,
            |file| image.write(file),
            |phase| self.checkpoint_boundary(phase),
        )?;
        state.bytes.clear();
        state.base = wal::Stamp {
            version: 3,
            id: image.id,
            sequence: image.sequence,
            checksum: image.root(),
            len: image.len() as u64,
        };
        state.paged = Some(image);
        state.wal_commits = 0;
        state.wal_bytes = disk.wal_len;
        state.paged.as_mut().unwrap().bind(&self.collections);
        self.checkpoint_boundary(CheckpointBoundary::SnapshotRenamed)?;
        atomic_snapshot(&wal::sidecar(&self.path, ".wal"), &wal::header(id))?;
        state.wal_bytes = wal::HEADER as u64;
        self.checkpoint_boundary(CheckpointBoundary::WalReset)?;
        Ok(Some(backup))
    }
    fn write_checkpoint(&self, state: &mut Persistence, bytes: Option<Vec<u8>>) -> Result<()> {
        atomic_snapshot_with(
            &self.path,
            bytes.as_deref().unwrap_or(&state.bytes),
            |phase| self.checkpoint_boundary(phase),
        )?;
        // The snapshot rename is durable even if resetting WAL fails. Keep
        // this handle aligned with disk so a retry is safe and not stale.
        if let Some(bytes) = bytes {
            state.bytes = bytes;
        }
        let id = storage::header(&state.bytes).0;
        state.wal_commits = 0;
        self.checkpoint_boundary(CheckpointBoundary::SnapshotRenamed)?;
        atomic_snapshot(&wal::sidecar(&self.path, ".wal"), &wal::header(id))?;
        state.wal_bytes = wal::HEADER as u64;
        self.checkpoint_boundary(CheckpointBoundary::WalReset)?;
        Ok(())
    }
    fn checkpoint_boundary(&self, phase: CheckpointBoundary) -> Result<()> {
        #[cfg(test)]
        tests::fault(self, phase)?;
        #[cfg(not(test))]
        let _ = phase;
        Ok(())
    }
}
fn absolute(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(fs::canonicalize(path)?);
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(fs::canonicalize(parent)?.join(
        path.file_name()
            .ok_or_else(|| Error::InvalidArgument("file name required".into()))?,
    ))
}
fn identity() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    time ^ ((std::process::id() as u128) << 64) ^ NEXT.fetch_add(1, Ordering::Relaxed) as u128
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CheckpointBoundary {
    RecoverySync,
    WalWritten,
    WalSynced,
    SnapshotWritten,
    SnapshotSynced,
    SnapshotPublished,
    SnapshotRenamed,
    WalReset,
}

fn atomic_snapshot(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_snapshot_with(path, bytes, |_| Ok(()))
}
fn atomic_snapshot_with(
    path: &Path,
    bytes: &[u8],
    boundary: impl Fn(CheckpointBoundary) -> Result<()>,
) -> Result<()> {
    atomic_snapshot_stream(path, |file| file.write_all(bytes), boundary)
}
fn atomic_snapshot_stream(
    path: &Path,
    write: impl FnOnce(&mut File) -> std::io::Result<()>,
    boundary: impl Fn(CheckpointBoundary) -> Result<()>,
) -> Result<()> {
    let tmp = wal::sidecar(path, &format!(".{}.tmp", identity()));
    let result = (|| {
        let permissions = match fs::metadata(path) {
            Ok(meta) => Some(meta.permissions()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        write(&mut file)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        boundary(CheckpointBoundary::SnapshotWritten)?;
        file.sync_all()?;
        boundary(CheckpointBoundary::SnapshotSynced)?;
        fs::rename(&tmp, path)?;
        boundary(CheckpointBoundary::SnapshotPublished)?;
        sync_parent_dir(path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}
#[cfg(unix)]
pub(crate) fn sync_parent_dir(path: &Path) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let dir = File::open(dir)?;
    if !dir.metadata()?.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            "snapshot parent is not a directory",
        ));
    }
    dir.sync_all()
}

// Windows has no portable unprivileged directory-fsync equivalent. File data
// is flushed before rename; process-crash recovery is tested separately from
// power-loss durability. Never silently discard an actual file flush error.
#[cfg(not(unix))]
pub(crate) fn sync_parent_dir(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
#[path = "database/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "database/paged_tests.rs"]
mod paged_tests;
