use std::fmt;
use std::io;
use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// The file exists but is not a valid Recern Vector database.
    Corrupt(String),
    /// The file was written by a newer, incompatible format version.
    UnsupportedVersion(u32),
    FileExists(PathBuf),
    CollectionExists(String),
    CollectionNotFound(String),
    DimensionMismatch {
        expected: usize,
        actual: usize,
    },
    InvalidVector(String),
    InvalidArgument(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(err) => write!(f, "I/O error: {err}"),
            Error::Corrupt(reason) => write!(f, "corrupt database file: {reason}"),
            Error::UnsupportedVersion(version) => {
                write!(f, "unsupported file format version {version}")
            }
            Error::FileExists(path) => write!(f, "file already exists: {}", path.display()),
            Error::CollectionExists(name) => write!(f, "collection already exists: {name}"),
            Error::CollectionNotFound(name) => write!(f, "collection not found: {name}"),
            Error::DimensionMismatch { expected, actual } => {
                write!(
                    f,
                    "expected a vector with {expected} dimensions, got {actual}"
                )
            }
            Error::InvalidVector(reason) => write!(f, "invalid vector: {reason}"),
            Error::InvalidArgument(reason) => write!(f, "invalid argument: {reason}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::Io(err)
    }
}
