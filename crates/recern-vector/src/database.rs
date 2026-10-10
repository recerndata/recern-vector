use crate::collection::{Collection, CollectionConfig};
use crate::error::{Error, Result};
use crate::{storage, wal};
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
    persistence: Mutex<Vec<u8>>,
    read_only: bool,
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
        let bytes = storage::encode(&collections, identity(), 0);
        atomic_snapshot(&path, &bytes)?;
        Ok(Self {
            path,
            collections,
            persistence: Mutex::new(bytes),
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
        Ok(Self {
            path,
            collections,
            persistence: Mutex::new(loaded.bytes),
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
        u32::from_le_bytes(bytes[4..8].try_into().expect("validated header"))
    }
    pub fn is_read_only(&self) -> bool {
        self.read_only
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

    /// Appends changed pages and fsyncs a commit. The base snapshot is unchanged
    /// except when migrating a format-1 database. No-op saves do not grow WAL.
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
        let mut previous = self
            .persistence
            .lock()
            .map_err(|_| Error::InvalidArgument("persistence lock poisoned".into()))?;
        let _lock = Lock::write(&self.path)?;
        let disk = wal::load(&self.path)?;
        if disk.bytes != *previous {
            return Err(Error::InvalidArgument(
                "database changed in another handle; reopen before writing".into(),
            ));
        }
        let (mut id, seq) = storage::header(&previous);
        let migrate = id == 0;
        if migrate {
            id = identity();
        }
        let unchanged = storage::encode(&self.collections, id, seq) == *previous;
        if unchanged && !checkpoint {
            return Ok(());
        }
        let next = if unchanged {
            seq
        } else {
            seq.checked_add(1)
                .ok_or_else(|| Error::InvalidArgument("commit sequence exhausted".into()))?
        };
        let bytes = storage::encode(&self.collections, id, next);
        if checkpoint || migrate {
            atomic_snapshot(&self.path, &bytes)?;
            // Once the rename is durable, old WAL commits are already included.
            // Update memory even if resetting the log fails; retry stays safe.
            *previous = bytes;
            atomic_snapshot(&wal::sidecar(&self.path, ".wal"), &wal::header(id))?;
            sync_parent_dir(&self.path);
        } else {
            let frame = wal::frame(&previous, &bytes, next);
            wal::append(&self.path, disk.wal_len, id, &frame)?;
            *previous = bytes;
        }
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
fn atomic_snapshot(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = wal::sidecar(path, &format!(".{}.tmp", identity()));
    let result = (|| {
        let mut file = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        sync_parent_dir(path);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}
pub(crate) fn sync_parent_dir(path: &Path) {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if let Ok(dir) = File::open(dir) {
        let _ = dir.sync_all();
    }
}
