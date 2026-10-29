use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Code {
    InvalidArgument,
    NotFound,
    AlreadyExists,
    Conflict,
    Unsupported,
    Unavailable,
    PermissionDenied,
    CorruptData,
}
#[derive(Debug, Clone, Serialize)]
pub struct Error {
    pub code: Code,
    pub request_id: Option<String>,
    pub write_outcome: &'static str,
}
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub(crate) fn new(code: Code) -> Self {
        Self {
            code,
            request_id: None,
            write_outcome: "not_applicable",
        }
    }
    pub(crate) fn invalid() -> Self {
        Self::new(Code::InvalidArgument)
    }
    pub(crate) fn corrupt() -> Self {
        Self::new(Code::CorruptData)
    }
    pub(crate) fn unavailable() -> Self {
        Self::new(Code::Unavailable)
    }
    pub(crate) fn write(mut self) -> Self {
        if self.write_outcome == "not_applicable" {
            self.write_outcome = "unknown";
        }
        self
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Native paths, URLs, credentials, metadata and proxy text stay private.
        write!(
            f,
            "Recern portable {:?} (write outcome: {})",
            self.code, self.write_outcome
        )
    }
}
impl std::error::Error for Error {}
impl From<recern_vector::Error> for Error {
    fn from(value: recern_vector::Error) -> Self {
        use recern_vector::Error as E;
        Self::new(match value {
            E::CollectionNotFound(_) => Code::NotFound,
            E::CollectionExists(_) | E::FileExists(_) => Code::AlreadyExists,
            E::UnsupportedVersion(_) => Code::Unsupported,
            E::Corrupt(_) => Code::CorruptData,
            E::Io(_) => Code::Unavailable,
            E::InvalidArgument(ref s) if s.contains("changed in another handle") => Code::Conflict,
            _ => Code::InvalidArgument,
        })
    }
}
