//! On-disk entry and hint-file layout for Bitcask data files.
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
//! See `docs/bitcask-implementation-plan.md` §1 for the full rationale.

use std::io::{self, Read};

pub const HEADER_SIZE: usize = 16;
pub const HINT_HEADER_SIZE: usize = 20;

const TOMBSTONE_BIT: u32 = 1 << 31;
const KSZ_MASK: u32 = !TOMBSTONE_BIT;

/// Largest key length representable in the 31 bits left over after stealing
/// the top bit of `ksz` for the tombstone flag.
pub const MAX_KEY_LEN: u32 = KSZ_MASK;

fn encode_ksz(key_len: u32, tombstone: bool) -> u32 {
    debug_assert!(
        key_len <= KSZ_MASK,
        "key too large ({key_len} bytes, max {KSZ_MASK})"
    );
    key_len | if tombstone { TOMBSTONE_BIT } else { 0 }
}

fn decode_ksz(raw: u32) -> (u32, bool) {
    (raw & KSZ_MASK, raw & TOMBSTONE_BIT != 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryHeader {
    pub tstamp: u32,
    pub ksz: u32,
    pub value_sz: u32,
    pub tombstone: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub header: EntryHeader,
    pub key: Vec<u8>,
    pub value: Vec<u8>, // empty for tombstones
}

/// A fully-encoded data-file entry (CRC + header + key + value), ready to
/// be appended verbatim to a `.bitcask.data` file.
///
/// Distinct from [`EncodedHint`] so the two can never be accidentally
/// swapped (e.g. appending a hint record into a data file) — the type
/// checker catches what a bare `Vec<u8>` return type would not.
///
/// Also carries `value_len`, the length of the trailing value portion,
/// since every caller that appends this buffer needs it (to compute
/// `value_pos` for the keydir) and re-deriving or re-threading it
/// separately alongside the bytes is exactly the kind of "two things that
/// must stay in sync" footgun a single owning type avoids.
#[derive(Debug, Clone, PartialEq, Eq)]
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
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
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
    let ksz_and_flags = encode_ksz(key.len() as u32, tombstone);
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
    let crc = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    let tstamp = u32::from_le_bytes(buf[4..8].try_into().unwrap());
    let ksz_and_flags = u32::from_le_bytes(buf[8..12].try_into().unwrap());
    let value_sz = u32::from_le_bytes(buf[12..16].try_into().unwrap());
    let (ksz, tombstone) = decode_ksz(ksz_and_flags);
    (
        crc,
        EntryHeader {
            tstamp,
            ksz,
            value_sz,
            tombstone,
        },
    )
}

/// Verify a stored CRC against the entry bytes that follow it (everything
/// after the CRC field: tstamp, ksz_and_flags, value_sz, key, value).
pub fn verify_crc(crc: u32, rest_of_entry: &[u8]) -> bool {
    crc32fast::hash(rest_of_entry) == crc
}

/// A fully-encoded hint-file record, ready to be appended verbatim to a
/// `.bitcask.hint` file.
///
/// Distinct from [`EncodedEntry`] — same shape (a length-prefixed byte
/// buffer) but a different on-disk destination and layout, so keeping them
/// as separate types prevents a hint record from ever being written into a
/// data file, or vice versa, by mistake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedHint(Vec<u8>);

impl EncodedHint {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl AsRef<[u8]> for EncodedHint {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Encode one hint-file record for `key`, whose data-file entry has the
/// given header and whose value starts at `value_pos` in the data file.
pub fn encode_hint(key: &[u8], header: &EntryHeader, value_pos: u64) -> EncodedHint {
    let ksz_and_flags = encode_ksz(header.ksz, header.tombstone);
    let mut buf = Vec::with_capacity(HINT_HEADER_SIZE + key.len());
    buf.extend_from_slice(&header.tstamp.to_le_bytes());
    buf.extend_from_slice(&ksz_and_flags.to_le_bytes());
    buf.extend_from_slice(&header.value_sz.to_le_bytes());
    buf.extend_from_slice(&value_pos.to_le_bytes());
    buf.extend_from_slice(key);
    EncodedHint(buf)
}

/// Decode the fixed-size hint header. Does not read the trailing key bytes.
pub fn decode_hint_header(buf: &[u8; HINT_HEADER_SIZE]) -> (EntryHeader, u64) {
    let tstamp = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    let ksz_and_flags = u32::from_le_bytes(buf[4..8].try_into().unwrap());
    let value_sz = u32::from_le_bytes(buf[8..12].try_into().unwrap());
    let value_pos = u64::from_le_bytes(buf[12..20].try_into().unwrap());
    let (ksz, tombstone) = decode_ksz(ksz_and_flags);
    (
        EntryHeader {
            tstamp,
            ksz,
            value_sz,
            tombstone,
        },
        value_pos,
    )
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

/// Read up to `buf.len()` bytes, stopping early only at EOF. Returns the
/// number of bytes actually read, which is `buf.len()` unless EOF was hit.
/// Needed because a single `Read::read` call is allowed to return short
/// reads for reasons other than EOF.
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
    let body_len = header.ksz as usize + header.value_sz as usize;
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

    let key = body[..header.ksz as usize].to_vec();
    let value = body[header.ksz as usize..].to_vec();
    Ok(Some(EntryRead::Ok(Entry { header, key, value }, total_len)))
}

/// Result of attempting to read one record from a hint file.
#[derive(Debug)]
pub enum HintRead {
    Ok {
        header: EntryHeader,
        value_pos: u64,
        key: Vec<u8>,
    },
    Truncated,
}

/// Read one record from a hint file `r`, which must already be positioned
/// at the start of the record. Returns `Ok(None)` on a clean EOF at a
/// record boundary.
pub fn read_hint<R: Read>(r: &mut R) -> io::Result<Option<HintRead>> {
    let mut header_buf = [0u8; HINT_HEADER_SIZE];
    let n = read_up_to(r, &mut header_buf)?;
    if n == 0 {
        return Ok(None);
    }
    if n < HINT_HEADER_SIZE {
        return Ok(Some(HintRead::Truncated));
    }
    let (header, value_pos) = decode_hint_header(&header_buf);
    let mut key = vec![0u8; header.ksz as usize];
    let n2 = read_up_to(r, &mut key)?;
    if n2 < key.len() {
        return Ok(Some(HintRead::Truncated));
    }
    Ok(Some(HintRead::Ok {
        header,
        value_pos,
        key,
    }))
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
            let read = read_entry(&mut cursor).unwrap().unwrap();
            let EntryRead::Ok(entry, total_len) = read else {
                panic!("expected Ok, got {read:?}");
            };
            assert_eq!(entry.key, key);
            assert_eq!(entry.value, value);
            assert_eq!(entry.header.tombstone, tombstone);
            assert_eq!(entry.header.tstamp, 1_700_000_000);
            assert_eq!(total_len, cursor.into_inner().len() as u64);
        }
    }

    #[test]
    fn hint_round_trip() {
        let header = EntryHeader {
            tstamp: 42,
            ksz: 3,
            value_sz: 10,
            tombstone: false,
        };
        let encoded = encode_hint(b"key", &header, 12345);
        let mut cursor = Cursor::new(encoded.into_bytes());
        let read = read_hint(&mut cursor).unwrap().unwrap();
        let HintRead::Ok {
            header: got_header,
            value_pos,
            key,
        } = read
        else {
            panic!("expected Ok");
        };
        assert_eq!(got_header, header);
        assert_eq!(value_pos, 12345);
        assert_eq!(key, b"key");
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
        let read = read_entry(&mut cursor).unwrap().unwrap();
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
        assert!(read_entry(&mut cursor).unwrap().is_none());

        let mut cursor = Cursor::new(Vec::<u8>::new());
        assert!(read_hint(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn ksz_flag_round_trip() {
        assert_eq!(decode_ksz(encode_ksz(0, false)), (0, false));
        assert_eq!(decode_ksz(encode_ksz(0, true)), (0, true));
        assert_eq!(decode_ksz(encode_ksz(MAX_KEY_LEN, false)), (MAX_KEY_LEN, false));
        assert_eq!(decode_ksz(encode_ksz(MAX_KEY_LEN, true)), (MAX_KEY_LEN, true));
    }
}
