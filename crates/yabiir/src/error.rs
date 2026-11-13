use std::fmt;

/// Errors surfaced by the [`crate::api::Bitcask`] API.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Underlying I/O failure (open/read/write/sync/rename/remove, etc.).
    Io(std::io::Error),
    /// A second `read_write` handle was opened on a directory that already
    /// has one open (in this process or another).
    AlreadyLocked,
    /// `put`/`delete` was called with a zero-length key.
    EmptyKey,
    /// The key is longer than an entry can record: more than 2³¹ − 1 bytes.
    KeyTooLarge,
    /// The value is longer than an entry can record: more than 2³² − 1
    /// bytes.
    ValueTooLarge,
    /// `put`, `delete` or `merge` was called on a handle opened read-only.
    ReadOnly,
    /// A range of keys (`Engine::scan`) was asked of a store whose keydir
    /// keeps no order — opened with `key_order: KeyOrder::Hashed`.
    Unordered,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::AlreadyLocked => {
                write!(f, "directory already locked for writing by another handle")
            }
            Error::EmptyKey => write!(f, "key must not be empty"),
            Error::KeyTooLarge => write!(f, "key too large: the limit is 2^31 - 1 bytes"),
            Error::ValueTooLarge => write!(f, "value too large: the limit is 2^32 - 1 bytes"),
            Error::ReadOnly => write!(f, "datastore is not open for writing (read-only handle)"),
            Error::Unordered => write!(
                f,
                "a range of keys needs a sorted keydir: open with key_order: KeyOrder::Sorted"
            ),
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
        let cases: [(Error, &str); 7] = [
            (Error::Io(std::io::Error::other("boom")), "boom"),
            (Error::AlreadyLocked, "locked"),
            (Error::EmptyKey, "empty"),
            (Error::ReadOnly, "read-only"),
            (Error::KeyTooLarge, "key too large"),
            (Error::ValueTooLarge, "value too large"),
            (Error::Unordered, "sorted keydir"),
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
