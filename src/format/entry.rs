//! `.bitcask.data` entry encoding/decoding. See the parent module
//! (`crate::format`) for the full on-disk byte layout this implements.

use std::io::{self, Read};

use super::{EntryHeader, decode_key_size, encode_key_size, read_up_to};

pub const HEADER_SIZE: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub header: EntryHeader,
    pub key: Vec<u8>,
    pub value: Vec<u8>, // empty for tombstones
}

impl Entry {
    pub(crate) fn is_tombstone(&self) -> bool {
        self
            .header
            .tombstone
    }
}

/// A fully-encoded data-file entry (CRC + header + key + value), ready to
/// be appended verbatim to a `.bitcask.data` file.
///
/// Distinct from [`super::EncodedHint`] so the two can never be accidentally
/// swapped (e.g. appending a hint record into a data file) — the type
/// checker catches what a bare `Vec<u8>` return type would not.
///
/// Also carries `value_len`, the length of the trailing value portion,
/// since every caller that appends this buffer needs it (to compute
/// `value_pos` for the keydir) and re-deriving or re-threading it
/// separately alongside the bytes is exactly the kind of "two things that
/// must stay in sync" footgun a single owning type avoids.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedEntry {
    bytes: Vec<u8>,
    value_len: usize,
}

impl EncodedEntry {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes
            .is_empty()
    }

    /// Length, in bytes, of the value portion at the tail of this buffer.
    pub fn value_len(&self) -> usize {
        self.value_len
    }
}

impl AsRef<[u8]> for EncodedEntry {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

/// Encode a full entry (header + key + value), CRC included, ready to be
/// appended to a data file.
///
/// # Panics
/// Panics (via `debug_assert!` in `encode_ksz`) in debug builds if
/// `key.len() > MAX_KEY_LEN`; release builds silently truncate the high bit
/// away, which the caller must not rely on — validate key length before
/// calling this in the engine layer.
pub fn encode_entry(key: &[u8], value: &[u8], tombstone: bool, tstamp: u32) -> EncodedEntry {
    let ksz_and_flags = encode_key_size(key.len() as u32, tombstone);
    let value_sz = value.len() as u32;

    let mut buf = Vec::with_capacity(HEADER_SIZE + key.len() + value.len());
    buf.extend_from_slice(&[0u8; 4]); // crc placeholder, filled in below
    buf.extend_from_slice(&tstamp.to_le_bytes());
    buf.extend_from_slice(&ksz_and_flags.to_le_bytes());
    buf.extend_from_slice(&value_sz.to_le_bytes());
    buf.extend_from_slice(key);
    buf.extend_from_slice(value);

    let crc = crc32fast::hash(&buf[4..]);
    buf[0..4].copy_from_slice(&crc.to_le_bytes());
    EncodedEntry {
        bytes: buf,
        value_len: value.len(),
    }
}

/// Decode the fixed-size entry header. Returns the stored CRC and the
/// parsed header fields; does not read or verify the key/value body.
pub fn decode_entry_header(buf: &[u8; HEADER_SIZE]) -> (u32, EntryHeader) {
    let crc = u32::from_le_bytes(
        buf[0..4]
            .try_into()
            .unwrap(),
    );
    let tstamp = u32::from_le_bytes(
        buf[4..8]
            .try_into()
            .unwrap(),
    );
    let ksz_and_flags = u32::from_le_bytes(
        buf[8..12]
            .try_into()
            .unwrap(),
    );
    let value_sz = u32::from_le_bytes(
        buf[12..16]
            .try_into()
            .unwrap(),
    );
    let (ksz, tombstone) = decode_key_size(ksz_and_flags);
    (
        crc,
        EntryHeader {
            timestamp: tstamp,
            key_size: ksz,
            value_size: value_sz,
            tombstone,
        },
    )
}

/// Verify a stored CRC against the entry bytes that follow it (everything
/// after the CRC field: tstamp, ksz_and_flags, value_sz, key, value).
pub fn verify_crc(crc: u32, rest_of_entry: &[u8]) -> bool {
    crc32fast::hash(rest_of_entry) == crc
}

/// Result of attempting to read one entry from a data file at a given
/// offset. Distinguishes a clean end-of-file (no more entries) from a torn
/// write (crash mid-append) and from a complete-but-corrupt entry
/// (bitrot), so recovery can handle each case correctly instead of
/// conflating them.
#[derive(Debug)]
pub enum EntryRead {
    /// A fully-decoded, CRC-valid entry, plus its total on-disk length
    /// (header + key + value) for advancing the scan cursor.
    Ok(Entry, u64),
    /// Fewer bytes were available than the header (or the header's declared
    /// body) requires — a torn write, expected at most at the tail of the
    /// most-recently-active file after an unclean shutdown.
    Truncated,
    /// A complete entry was read but its CRC did not match — on-disk
    /// corruption of an otherwise well-formed entry. Carries the entry's
    /// total length so the scan can skip past it and keep going.
    CrcMismatch { total_len: u64 },
}

/// Read one entry from `r`, which must already be positioned at the start
/// of the entry. Returns `Ok(None)` on a clean EOF exactly at an entry
/// boundary (nothing left to read — the normal end of a scan), or
/// `Ok(Some(EntryRead::..))` describing what was found.
///
/// Does not seek; callers that need random access (as opposed to
/// sequential scanning) should seek `r` to the desired offset first.
pub fn read_entry<R: Read>(r: &mut R) -> io::Result<Option<EntryRead>> {
    let mut header_buf = [0u8; HEADER_SIZE];
    let n = read_up_to(r, &mut header_buf)?;
    if n == 0 {
        return Ok(None);
    }
    if n < HEADER_SIZE {
        return Ok(Some(EntryRead::Truncated));
    }
    let (crc, header) = decode_entry_header(&header_buf);
    let body_len = header.key_size as usize + header.value_size as usize;
    let mut body = vec![0u8; body_len];
    let n2 = read_up_to(r, &mut body)?;
    if n2 < body_len {
        return Ok(Some(EntryRead::Truncated));
    }
    let total_len = (HEADER_SIZE + body_len) as u64;

    let mut crc_input = Vec::with_capacity(HEADER_SIZE - 4 + body_len);
    crc_input.extend_from_slice(&header_buf[4..]);
    crc_input.extend_from_slice(&body);
    if !verify_crc(crc, &crc_input) {
        return Ok(Some(EntryRead::CrcMismatch { total_len }));
    }

    let key = body[..header.key_size as usize].to_vec();
    let value = body[header.key_size as usize..].to_vec();
    Ok(Some(EntryRead::Ok(Entry { header, key, value }, total_len)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn entry_round_trip() {
        for (key, value, tombstone) in [
            (b"k".to_vec(), b"v".to_vec(), false),
            (b"empty-value".to_vec(), Vec::new(), false),
            (b"tombstoned".to_vec(), Vec::new(), true),
            (b"large".to_vec(), vec![7u8; 1024 * 1024], false),
        ] {
            let encoded = encode_entry(&key, &value, tombstone, 1_700_000_000);
            assert_eq!(encoded.value_len(), value.len());
            let mut cursor = Cursor::new(encoded.into_bytes());
            let read = read_entry(&mut cursor)
                .unwrap()
                .unwrap();
            let EntryRead::Ok(entry, total_len) = read else {
                panic!("expected Ok, got {read:?}");
            };
            assert_eq!(entry.key, key);
            assert_eq!(entry.value, value);
            assert_eq!(
                entry
                    .header
                    .tombstone,
                tombstone
            );
            assert_eq!(
                entry
                    .header
                    .timestamp,
                1_700_000_000
            );
            assert_eq!(
                total_len,
                cursor
                    .into_inner()
                    .len() as u64
            );
        }
    }

    #[test]
    fn corrupt_crc_is_detected() {
        let mut encoded = encode_entry(b"key", b"value", false, 0).into_bytes();
        // flip a bit inside the body, past the header, leaving the header
        // (and hence declared lengths) intact so this is a same-length,
        // same-shape but corrupt entry.
        let last = encoded.len() - 1;
        encoded[last] ^= 0xFF;

        let mut cursor = Cursor::new(encoded);
        let read = read_entry(&mut cursor)
            .unwrap()
            .unwrap();
        match read {
            EntryRead::CrcMismatch { total_len } => assert_eq!(total_len, HEADER_SIZE as u64 + 8),
            other => panic!("expected CrcMismatch, got {other:?}"),
        }
    }

    #[test]
    fn truncation_is_detected_at_every_boundary() {
        let encoded = encode_entry(b"key", b"value", false, 0).into_bytes();
        for cut in 1..encoded.len() {
            let mut cursor = Cursor::new(encoded[..cut].to_vec());
            let read = read_entry(&mut cursor).unwrap();
            match read {
                Some(EntryRead::Truncated) => {}
                other => panic!("cut at {cut}: expected Truncated, got {other:?}"),
            }
        }
    }

    #[test]
    fn clean_eof_at_boundary_is_none() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        assert!(
            read_entry(&mut cursor)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn is_tombstone_reflects_the_header_flag() {
        let live = encode_entry(b"k", b"v", false, 0);
        let dead = encode_entry(b"k", &[], true, 0);
        let EntryRead::Ok(live_entry, _) = read_entry(&mut Cursor::new(live.into_bytes()))
            .unwrap()
            .unwrap()
        else {
            panic!("expected Ok");
        };
        let EntryRead::Ok(dead_entry, _) = read_entry(&mut Cursor::new(dead.into_bytes()))
            .unwrap()
            .unwrap()
        else {
            panic!("expected Ok");
        };
        assert!(!live_entry.is_tombstone());
        assert!(dead_entry.is_tombstone());
    }

    #[test]
    fn encoded_entry_len_is_empty_and_as_ref_match_the_real_bytes() {
        let encoded = encode_entry(b"key", b"value", false, 0);
        let expected_len = HEADER_SIZE + 3 + 5;
        assert_eq!(encoded.len(), expected_len);
        assert!(!encoded.is_empty());
        assert_eq!(encoded.as_ref(), encoded.as_bytes());
        assert_eq!(encoded.as_ref().len(), expected_len);
    }
}
