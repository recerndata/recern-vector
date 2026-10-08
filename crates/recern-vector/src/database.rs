use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::collection::{Collection, CollectionConfig};
use crate::error::{Error, Result};
use crate::storage;

/// A database file and the collections it holds.
///
/// The whole database is loaded into memory on open. Changes stay in memory
/// until [`Database::save`], which atomically replaces the file.
pub struct Database {
    path: PathBuf,
    collections: BTreeMap<String, Collection>,
}

impl Database {
    /// Creates a new, empty database file. Fails if the file exists.
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if path.exists() {
            return Err(Error::FileExists(path));
        }
        let db = Self {
            path,
            collections: BTreeMap::new(),
        };
        db.save()?;
        Ok(db)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let collections = storage::decode(&fs::read(&path)?)?;
        Ok(Self { path, collections })
    }

    pub fn open_or_create(path: impl AsRef<Path>) -> Result<Self> {
        if path.as_ref().exists() {
            Self::open(path)
        } else {
            Self::create(path)
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
        self.collections
            .get_mut(name)
            .ok_or_else(|| Error::CollectionNotFound(name.to_owned()))
    }

    pub fn drop_collection(&mut self, name: &str) -> Result<()> {
        self.collections
            .remove(name)
            .map(|_| ())
            .ok_or_else(|| Error::CollectionNotFound(name.to_owned()))
    }

    pub fn collections(&self) -> impl Iterator<Item = &Collection> {
        self.collections.values()
    }

    /// Writes the database to a temporary file, syncs it, and renames it over
    /// the original, so a crash leaves either the old or the new version.
    pub fn save(&self) -> Result<()> {
        let bytes = storage::encode(&self.collections);
        let mut tmp = self.path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        {
            let mut file = File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        sync_parent_dir(&self.path);
        Ok(())
    }
}

/// Persists the rename itself. Not supported on every platform, so failures
/// are ignored.
fn sync_parent_dir(path: &Path) {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    if let Ok(dir) = File::open(dir) {
        let _ = dir.sync_all();
    }
}
