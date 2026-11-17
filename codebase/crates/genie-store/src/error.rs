use std::path::PathBuf;

/// Errors from the store. Callers in `genie-core` map these to `Defect` (01 §7):
/// a store failure is never the model's or the user's fault.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// AEAD failure: wrong key, tampered or truncated data.
    #[error("decryption failed: {0}")]
    Crypto(&'static str),
    #[error("malformed data: {0}")]
    Format(String),
    /// The DB was written by a newer Genie (04 §5: downgrades are not supported).
    #[error("Update Genie to open this data (schema migration {found} is newer than {known})")]
    NewerSchema { found: i64, known: i64 },
    /// The key does not open the DB.
    #[error("the data key does not open {0}")]
    WrongKey(PathBuf),
    /// The DB exists but the keychain has no data key (04 §2 "key loss").
    #[error("data key missing from the keychain (recovery key available: {recovery_available})")]
    KeyMissing { recovery_available: bool },
    #[error("keychain: {0}")]
    KeyStore(String),
    #[error("{0} not found")]
    NotFound(String),
    /// Durable seqs are gap-free (02 §3.1).
    #[error("seq {got} for {session} is not head_seq + 1 ({head})")]
    SeqGap {
        session: String,
        head: u64,
        got: u64,
    },
    /// The requested cursor is below the retained log (02 §9).
    #[error("events of {session} up to {floor} were pruned")]
    BelowFloor { session: String, floor: u64 },
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;
