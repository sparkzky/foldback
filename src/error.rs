use thiserror::Error;

#[derive(Debug, Error)]
pub enum FoldbackError {
    #[error("ref not found: {ref_id}")]
    NotFound { ref_id: String },
    #[error("ref expired: {ref_id}")]
    Expired { ref_id: String },
    #[error("invalid ref: {input}")]
    InvalidRef { input: String },
    #[error("storage error: {0}")]
    Storage(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("bad input: {0}")]
    BadInput(String),
    #[error("blob integrity failure (size or sha256 mismatch): ref={ref_id} channel={channel}")]
    Corrupted { ref_id: String, channel: String },
}

impl FoldbackError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NotFound { .. } | Self::Expired { .. } => 1,
            Self::InvalidRef { .. } | Self::BadInput(_) => 2,
            Self::Storage(_) | Self::Io(_) | Self::Corrupted { .. } => 3,
        }
    }
}

impl From<rusqlite::Error> for FoldbackError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Storage(e.to_string())
    }
}
