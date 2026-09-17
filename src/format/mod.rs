//! On-disk entry and hint-file layout for Bitcask data files.
//!
//! This module is split into two submodules along the on-disk artifact each
//! one governs — `entry` for `.bitcask.data` records and `hint` for
//! `.bitcask.hint` records — but both submodules are private and everything
//! public is re-exported here, so callers only ever reach this as
//! `format::...`, never `format::entry::...` or `format::hint::...`.
//!
//! Entry layout (16-byte header, all integers little-endian):
//!
//! ```text
//! offset  size  field
//! 0       4     crc            CRC32 of bytes [4..end) of this entry
//! 4       4     tstamp         unix seconds, internal-only
//! 8       4     ksz_and_flags  bit 31 = tombstone flag, bits 0..30 = key length
//! 12      4     value_sz       0 for tombstones (deletes carry no payload)
//! 16      ksz   key
//! 16+ksz  value_sz  value
//! ```
//!
//! Hint file layout (20-byte header, no CRC — hint files are a rebuildable
//! cache; a corrupt or missing one just falls back to scanning the data
//! file):
//!
//! ```text
//! offset  size  field
//! 0       4     tstamp
//! 4       4     ksz_and_flags  (same encoding as the data file header)
//! 8       4     value_sz
//! 12      8     value_pos      position of the VALUE bytes in the data file
//! 20      ksz   key
//! ```
//!
//! See `docs/bitcask-implementation-plan.md` §1 for the full rationale,
//! including why the tombstone flag lives in the top bit of `ksz` rather
//! than a dedicated flags byte or a magic length sentinel.

mod entry;
mod hint;

pub use entry::{
    EncodedEntry, Entry, EntryRead, HEADER_SIZE, decode_entry_header, encode_entry, read_entry,
    verify_crc,
};
pub use hint::{
    EncodedHint, HINT_HEADER_SIZE, HintRead, decode_hint_header, encode_hint, read_hint,
};

use std::io::{self, Read};

const TOMBSTONE_BIT: u32 = 1 << 31;
const KSZ_MASK: u32 = !TOMBSTONE_BIT;

/// Largest key length representable in the 31 bits left over after stealing
/// the top bit of `ksz` for the tombstone flag.
pub const MAX_KEY_LEN: u32 = KSZ_MASK;

/// Header fields shared by both the data-file entry format and the
/// hint-file record format (everything except the CRC and, for hints, the
/// `value_pos` that only a hint record carries).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntryHeader {
    pub tstamp: u32,
    pub ksz: u32,
    pub value_sz: u32,
    pub tombstone: bool,
}

/// Pack a real key length and the tombstone flag into one `u32`, per §1.2 of
/// the implementation plan: the top bit is the flag, the low 31 bits are the
/// key length. Shared by `entry` and `hint` since both formats encode
/// `ksz` the same way.
fn encode_ksz(key_len: u32, tombstone: bool) -> u32 {
    debug_assert!(
        key_len <= KSZ_MASK,
        "key too large ({key_len} bytes, max {KSZ_MASK})"
    );
    key_len | if tombstone { TOMBSTONE_BIT } else { 0 }
}

/// Inverse of [`encode_ksz`].
fn decode_ksz(raw: u32) -> (u32, bool) {
    (raw & KSZ_MASK, raw & TOMBSTONE_BIT != 0)
}

/// Read up to `buf.len()` bytes, stopping early only at EOF. Returns the
/// number of bytes actually read, which is `buf.len()` unless EOF was hit.
/// Needed because a single `Read::read` call is allowed to return short
/// reads for reasons other than EOF. Shared by `entry::read_entry` and
/// `hint::read_hint`.
fn read_up_to<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match r.read(&mut buf[total..])? {
            0 => break,
            n => total += n,
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ksz_flag_round_trip() {
        assert_eq!(decode_ksz(encode_ksz(0, false)), (0, false));
        assert_eq!(decode_ksz(encode_ksz(0, true)), (0, true));
        assert_eq!(
            decode_ksz(encode_ksz(MAX_KEY_LEN, false)),
            (MAX_KEY_LEN, false)
        );
        assert_eq!(
            decode_ksz(encode_ksz(MAX_KEY_LEN, true)),
            (MAX_KEY_LEN, true)
        );
    }
}
