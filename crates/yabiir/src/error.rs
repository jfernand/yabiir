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
    /// The call names a real, planned part of the design
    /// (`docs/bitcask-implementation-plan.md`) that this milestone's
    /// implementation doesn't provide yet — e.g. `merge` before §7 lands.
    NotImplemented(&'static str),
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
            Error::NotImplemented(what) => write!(f, "not implemented yet: {what}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_produces_a_nonempty_message_naming_the_variant() {
        let cases: [(Error, &str); 6] = [
            (Error::Io(std::io::Error::other("boom")), "boom"),
            (Error::AlreadyLocked, "locked"),
            (Error::EmptyKey, "empty"),
            (Error::Corrupt("bad crc".to_string()), "bad crc"),
            (Error::ReadOnly, "read-only"),
            (Error::NotImplemented("merge"), "merge"),
        ];
        for (err, needle) in cases {
            let msg = err.to_string();
            assert!(!msg.is_empty(), "{err:?} produced an empty Display message");
            assert!(
                msg.contains(needle),
                "{err:?}'s Display message {msg:?} should mention {needle:?}"
            );
        }
    }

    #[test]
    fn source_chains_to_the_inner_io_error_only_for_the_io_variant() {
        use std::error::Error as _;
        let io_err = Error::Io(std::io::Error::other("boom"));
        assert!(
            io_err
                .source()
                .is_some()
        );
        assert!(
            Error::EmptyKey
                .source()
                .is_none()
        );
        assert!(
            Error::ReadOnly
                .source()
                .is_none()
        );
    }
}
