use serde_json::{Value, json};

pub type Result<T> = std::result::Result<T, ApiError>;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: u16,
    pub code: &'static str,
    pub reason: &'static str,
    pub outcome: &'static str,
}

impl ApiError {
    pub fn new(status: u16, code: &'static str, reason: &'static str) -> Self {
        Self {
            status,
            code,
            reason,
            outcome: "not_committed",
        }
    }
    pub fn invalid() -> Self {
        Self::new(400, "InvalidArgument", "invalid_argument")
    }
    pub fn conflict(reason: &'static str) -> Self {
        Self::new(409, "Conflict", reason)
    }
    pub fn missing() -> Self {
        Self::new(404, "NotFound", "not_found")
    }
    pub fn unavailable() -> Self {
        Self::new(503, "Unavailable", "unavailable")
    }
    pub fn too_large() -> Self {
        Self::new(413, "Unsupported", "response_too_large")
    }
    pub fn readonly(mut self) -> Self {
        self.outcome = "not_applicable";
        self
    }
    pub fn unknown(mut self) -> Self {
        self.outcome = "unknown";
        self
    }
    pub fn body(&self, request_id: &str) -> Value {
        json!({"error":{"code":self.code,"reason":self.reason,
            "message":"The operation could not be completed.",
            "request_id":request_id,"write_outcome":self.outcome}})
    }
}

impl From<recern_vector::Error> for ApiError {
    fn from(error: recern_vector::Error) -> Self {
        use recern_vector::Error;
        match error {
            Error::CollectionNotFound(_) => Self::missing(),
            Error::CollectionExists(_) => Self::new(409, "AlreadyExists", "already_exists"),
            Error::Corrupt(_) => Self::new(500, "CorruptData", "corrupt_data"),
            Error::UnsupportedVersion(_) => Self::new(422, "Unsupported", "unsupported_format"),
            Error::Io(_) | Error::FileExists(_) => Self::unavailable(),
            Error::InvalidArgument(ref message)
                if message.contains("changed in another handle") =>
            {
                Self::conflict("external_writer")
            }
            _ => Self::invalid(),
        }
    }
}
