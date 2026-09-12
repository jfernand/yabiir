use std::fmt;

/// Errors surfaced by the [`crate::api::Bitcask`] API.
#[derive(Debug)]
pub enum Error {
    /// Underlying I/O failure (open/read/write/sync/rename/remove, etc.).
    Io(std::io::Error),
    /// A second `read_write` handle was opened on a directory that already
    /// has one open (in this process or another).
    AlreadyLocked,
    /// `put`/`delete` was called with a zero-length key.
    EmptyKey,
    /// A data or hint file failed CRC verification or had an unrecognized
    /// layout that could not be explained by a clean truncation.
    Corrupt(String),
    /// A mutating call (`put`/`delete`/`merge`/`sync`) was made on a handle
    /// opened with `read_write: false`.
    ReadOnly,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::AlreadyLocked => {
                write!(f, "directory already locked for writing by another handle")
            }
            Error::EmptyKey => write!(f, "key must not be empty"),
            Error::Corrupt(msg) => write!(f, "data file corrupt: {msg}"),
            Error::ReadOnly => write!(f, "datastore is not open for writing (read-only handle)"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
