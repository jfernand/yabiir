//! The write path: `put` and `delete`.

use std::time::Instant;

use super::Engine;
use crate::error::{Error, Result};
use crate::format::{self, MAX_KEY_LEN, MAX_VALUE_LEN};
use crate::keydir::KeydirEntry;

impl Engine {
    /// [`Bitcask::put`](crate::Bitcask::put), with
    /// `between_append_and_publish` run after the entry is appended and
    /// before the keydir points at it, under the writer's lock. Production
    /// passes a no-op; tests try to run a merge or another write there, and
    /// check that it has to wait until the publish is done.
    pub(super) fn put_with_hook(
        &self,
        key: &[u8],
        value: &[u8],
        timestamp: u32,
        between_append_and_publish: impl FnOnce(),
    ) -> Result<()> {
        check_key(key)?;
        check_value(value)?;
        let writer = self.writer()?;
        let start = Instant::now();

        let encoded = format::encode_entry(key, value, false, timestamp);
        writer.append(&encoded, |file_id, value_pos| {
            between_append_and_publish();
            self.keydir
                .insert(
                    key,
                    KeydirEntry {
                        file_id,
                        value_size: value.len() as u32,
                        value_pos,
                        timestamp,
                    },
                );
        })?;
        self.metrics
            .record_put(start.elapsed());
        Ok(())
    }

    /// [`Bitcask::delete`](crate::Bitcask::delete), with
    /// `between_append_and_publish` run after the tombstone is appended and
    /// before the key leaves the keydir: the same window as
    /// [`Engine::put_with_hook`].
    pub(super) fn delete_with_hook(
        &self,
        key: &[u8],
        timestamp: u32,
        between_append_and_publish: impl FnOnce(),
    ) -> Result<()> {
        check_key(key)?;
        let writer = self.writer()?;
        let start = Instant::now();

        // A key that isn't there needs no tombstone. If a put of this key is
        // in flight and hasn't published yet, the delete counts as having
        // come first, and the put stands.
        let is_present = self
            .keydir
            .get(key)
            .is_some();
        if is_present {
            let tombstone = format::encode_entry(key, &[], true, timestamp);
            writer.append(&tombstone, |_, _| {
                between_append_and_publish();
                self.keydir
                    .remove(key);
            })?;
        }
        self.metrics
            .record_delete(start.elapsed());
        Ok(())
    }
}

fn check_key(key: &[u8]) -> Result<()> {
    check_key_len(key.len())
}

fn check_value(value: &[u8]) -> Result<()> {
    check_value_len(value.len())
}

/// The entry header stores the key's length in 31 bits.
pub(crate) fn check_key_len(len: usize) -> Result<()> {
    if len == 0 {
        Err(Error::EmptyKey)
    } else if len as u64 > MAX_KEY_LEN as u64 {
        Err(Error::KeyTooLarge)
    } else {
        Ok(())
    }
}

/// The entry header stores the value's length in 32 bits.
pub(crate) fn check_value_len(len: usize) -> Result<()> {
    if len as u64 > MAX_VALUE_LEN as u64 {
        Err(Error::ValueTooLarge)
    } else {
        Ok(())
    }
}
