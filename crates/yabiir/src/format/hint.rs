//! `.bitcask.hint` record encoding/decoding. See the parent module
//! (`crate::format`) for the full on-disk byte layout this implements.
//!
//! Individual hint records carry no CRC of their own — matching upstream
//! Bitcask's own design (`bitcask_fileops.erl`), not an oversight. Instead
//! a finished hint file ends with a 4-byte trailer: the CRC32 of every
//! record's bytes concatenated (see [`HINT_TRAILER_SIZE`] /
//! [`verify_hint_file`]). A hint file is only ever trusted as a whole —
//! `crate::recovery` verifies this trailer before parsing a single record,
//! and falls back to a full scan of the data file on any failure (missing
//! trailer, mismatched CRC, or an out-of-bounds pointer), the same way
//! upstream does.

use std::io::{self, Read};

use super::{EntryHeader, decode_key_size, encode_key_size, read_up_to};

pub const HINT_HEADER_SIZE: usize = 20;
/// Size of the whole-file trailing CRC32 appended after a hint file's last
/// record. See this module's doc comment.
pub const HINT_TRAILER_SIZE: usize = 4;

/// A fully-encoded hint-file record, ready to be appended verbatim to a
/// `.bitcask.hint` file.
///
/// Distinct from [`super::EncodedEntry`] — same shape (a length-prefixed
/// byte buffer) but a different on-disk destination and layout, so keeping
/// them as separate types prevents a hint record from ever being written
/// into a data file, or vice versa, by mistake.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedHint(Vec<u8>);

impl EncodedHint {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    pub fn len(&self) -> usize {
        self.0
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.0
            .is_empty()
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
    let ksz_and_flags = encode_key_size(header.key_size, header.tombstone);
    let mut buf = Vec::with_capacity(HINT_HEADER_SIZE + key.len());
    buf.extend_from_slice(
        &header
            .timestamp
            .to_le_bytes(),
    );
    buf.extend_from_slice(&ksz_and_flags.to_le_bytes());
    buf.extend_from_slice(
        &header
            .value_size
            .to_le_bytes(),
    );
    buf.extend_from_slice(&value_pos.to_le_bytes());
    buf.extend_from_slice(key);
    EncodedHint(buf)
}

/// Decode the fixed-size hint header. Does not read the trailing key bytes.
pub fn decode_hint_header(buf: &[u8; HINT_HEADER_SIZE]) -> (EntryHeader, u64) {
    let tstamp = u32::from_le_bytes(
        buf[0..4]
            .try_into()
            .unwrap(),
    );
    let ksz_and_flags = u32::from_le_bytes(
        buf[4..8]
            .try_into()
            .unwrap(),
    );
    let value_sz = u32::from_le_bytes(
        buf[8..12]
            .try_into()
            .unwrap(),
    );
    let value_pos = u64::from_le_bytes(
        buf[12..20]
            .try_into()
            .unwrap(),
    );
    let (ksz, tombstone) = decode_key_size(ksz_and_flags);
    (
        EntryHeader {
            timestamp: tstamp,
            key_size: ksz,
            value_size: value_sz,
            tombstone,
        },
        value_pos,
    )
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
    let mut key = vec![0u8; header.key_size as usize];
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

/// Verify a whole hint file's trailing CRC32 (the last
/// [`HINT_TRAILER_SIZE`] bytes) against everything before it. Returns the
/// record content with the trailer stripped off on success, `None` if the
/// file is too short to even contain a trailer or the CRC doesn't match —
/// either way, the caller should treat the whole file as untrustworthy,
/// not attempt to parse any records from it.
pub fn verify_hint_file(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.len() < HINT_TRAILER_SIZE {
        return None;
    }
    let (content, trailer) = bytes.split_at(bytes.len() - HINT_TRAILER_SIZE);
    let expected = u32::from_le_bytes(
        trailer
            .try_into()
            .unwrap(),
    );
    if crc32fast::hash(content) == expected {
        Some(content)
    } else {
        None
    }
}

/// Size of a version 2 hint record's fixed part: sequence number,
/// timestamp, key length (with the tombstone flag), value length and value
/// position.
pub const HINT_V2_HEADER_SIZE: usize = 28;

/// One hint record of either version, decoded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HintRecord {
    /// The sequence number of the write that the data-file entry records. A
    /// version 1 hint carries none, and its records read as 1.
    pub seq: u64,
    pub timestamp: u32,
    pub value_size: u32,
    pub tombstone: bool,
    /// Where the value starts in the data file.
    pub value_pos: u64,
    pub key: Vec<u8>,
}

/// Encode one version 2 hint record. A version 2 hint file is
/// [`HINT_MAGIC`](super::HINT_MAGIC), then these records, then the
/// whole-file CRC trailer over everything before it.
pub fn encode_hint_v2(record: &HintRecord) -> EncodedHint {
    let ksz_and_flags = encode_key_size(record.key.len() as u32, record.tombstone);
    let mut buf = Vec::with_capacity(HINT_V2_HEADER_SIZE + record.key.len());
    buf.extend_from_slice(&record.seq.to_le_bytes());
    buf.extend_from_slice(&record.timestamp.to_le_bytes());
    buf.extend_from_slice(&ksz_and_flags.to_le_bytes());
    buf.extend_from_slice(&record.value_size.to_le_bytes());
    buf.extend_from_slice(&record.value_pos.to_le_bytes());
    buf.extend_from_slice(&record.key);
    EncodedHint(buf)
}

/// Every record of a whole hint file of either version, or why the file
/// can't be trusted. The trailer's CRC is checked before any record is
/// parsed.
pub fn decode_hint_file(bytes: &[u8]) -> Result<Vec<HintRecord>, &'static str> {
    let content = verify_hint_file(bytes).ok_or("failed its whole-file CRC check")?;
    match content.strip_prefix(&super::HINT_MAGIC) {
        Some(records) => decode_v2_records(records),
        None => decode_v1_records(content),
    }
}

fn decode_v2_records(mut rest: &[u8]) -> Result<Vec<HintRecord>, &'static str> {
    const TRUNCATED: &str = "has a truncated record despite a valid CRC";
    let mut records = Vec::new();
    while !rest.is_empty() {
        let header = rest
            .get(..HINT_V2_HEADER_SIZE)
            .ok_or(TRUNCATED)?;
        let field = |at: usize| {
            u32::from_le_bytes(
                header[at..at + 4]
                    .try_into()
                    .unwrap(),
            )
        };
        let seq = u64::from_le_bytes(
            header[0..8]
                .try_into()
                .unwrap(),
        );
        let (key_len, tombstone) = decode_key_size(field(12));
        let value_pos = u64::from_le_bytes(
            header[20..28]
                .try_into()
                .unwrap(),
        );
        let key_end = HINT_V2_HEADER_SIZE
            .checked_add(key_len as usize)
            .ok_or(TRUNCATED)?;
        let key = rest
            .get(HINT_V2_HEADER_SIZE..key_end)
            .ok_or(TRUNCATED)?;
        records.push(HintRecord {
            seq,
            timestamp: field(8),
            value_size: field(16),
            tombstone,
            value_pos,
            key: key.to_vec(),
        });
        rest = &rest[key_end..];
    }
    Ok(records)
}

fn decode_v1_records(content: &[u8]) -> Result<Vec<HintRecord>, &'static str> {
    let mut records = Vec::new();
    let mut cursor = io::Cursor::new(content);
    // Reading from memory can't fail, hence the `expect`.
    while let Some(read) = read_hint(&mut cursor).expect("reading from memory") {
        let HintRead::Ok {
            header,
            value_pos,
            key,
        } = read
        else {
            return Err("has a truncated record despite a valid CRC");
        };
        records.push(HintRecord {
            seq: 1,
            timestamp: header.timestamp,
            value_size: header.value_size,
            tombstone: header.tombstone,
            value_pos,
            key,
        });
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn hint_round_trip() {
        let header = EntryHeader {
            timestamp: 42,
            key_size: 3,
            value_size: 10,
            tombstone: false,
        };
        let encoded = encode_hint(b"key", &header, 12345);
        let mut cursor = Cursor::new(encoded.into_bytes());
        let read = read_hint(&mut cursor)
            .unwrap()
            .unwrap();
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
    fn clean_eof_at_boundary_is_none() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        assert!(
            read_hint(&mut cursor)
                .unwrap()
                .is_none()
        );
    }

    /// Matches `entry.rs`'s `truncation_is_detected_at_every_boundary` —
    /// hint records need the same guarantee, and previously had no direct
    /// test for it.
    #[test]
    fn truncation_is_detected_at_every_boundary() {
        let header = EntryHeader {
            timestamp: 42,
            key_size: 3,
            value_size: 10,
            tombstone: false,
        };
        let encoded = encode_hint(b"key", &header, 12345).into_bytes();
        for cut in 1..encoded.len() {
            let mut cursor = Cursor::new(encoded[..cut].to_vec());
            let read = read_hint(&mut cursor).unwrap();
            match read {
                Some(HintRead::Truncated) => {}
                other => panic!("cut at {cut}: expected Truncated, got {other:?}"),
            }
        }
    }

    #[test]
    fn verify_hint_file_accepts_a_correctly_trailered_file() {
        let header = EntryHeader {
            timestamp: 1,
            key_size: 1,
            value_size: 1,
            tombstone: false,
        };
        let mut content = encode_hint(b"a", &header, 0).into_bytes();
        content.extend_from_slice(&encode_hint(b"b", &header, 1).into_bytes());
        let trailer = crc32fast::hash(&content).to_le_bytes();
        let mut file_bytes = content.clone();
        file_bytes.extend_from_slice(&trailer);

        let verified = verify_hint_file(&file_bytes).expect("valid trailer should verify");
        assert_eq!(verified, content.as_slice());
    }

    #[test]
    fn verify_hint_file_rejects_corrupted_content() {
        let header = EntryHeader {
            timestamp: 1,
            key_size: 1,
            value_size: 1,
            tombstone: false,
        };
        let content = encode_hint(b"a", &header, 0).into_bytes();
        let trailer = crc32fast::hash(&content).to_le_bytes();
        let mut file_bytes = content;
        file_bytes.extend_from_slice(&trailer);
        file_bytes[0] ^= 0xFF; // flip a bit inside a record, after computing the trailer

        assert!(verify_hint_file(&file_bytes).is_none());
    }

    #[test]
    fn verify_hint_file_rejects_a_file_too_short_to_hold_a_trailer() {
        assert!(verify_hint_file(&[0u8; HINT_TRAILER_SIZE - 1]).is_none());
    }

    #[test]
    fn verify_hint_file_accepts_an_empty_hint_file() {
        // A merge input with no live entries at all still finishes its
        // (empty) hint file with a trailer — the CRC of zero bytes.
        let trailer = crc32fast::hash(&[]).to_le_bytes();
        assert_eq!(verify_hint_file(&trailer), Some(&[][..]));
    }

    #[test]
    fn encoded_hint_len_is_empty_and_as_ref_match_the_real_bytes() {
        let header = EntryHeader {
            timestamp: 0,
            key_size: 3,
            value_size: 0,
            tombstone: false,
        };
        let encoded = encode_hint(b"key", &header, 0);
        let expected_len = HINT_HEADER_SIZE + 3;
        assert_eq!(encoded.len(), expected_len);
        assert!(!encoded.is_empty());
        assert_eq!(encoded.as_ref(), encoded.as_bytes());
        assert_eq!(
            encoded
                .as_ref()
                .len(),
            expected_len
        );
    }

    fn v2_record(key: &[u8], seq: u64) -> HintRecord {
        HintRecord {
            seq,
            timestamp: 7,
            value_size: 5,
            tombstone: false,
            value_pos: seq.wrapping_add(1000),
            key: key.to_vec(),
        }
    }

    fn with_trailer(mut content: Vec<u8>) -> Vec<u8> {
        let crc = crc32fast::hash(&content);
        content.extend_from_slice(&crc.to_le_bytes());
        content
    }

    #[test]
    fn a_v2_hint_file_decodes_to_its_records() {
        let records = vec![v2_record(b"a", 1), v2_record(b"bb", u64::MAX), v2_record(b"", 3)];
        let mut content = crate::format::HINT_MAGIC.to_vec();
        for r in &records {
            content.extend_from_slice(encode_hint_v2(r).as_bytes());
        }
        assert_eq!(decode_hint_file(&with_trailer(content)), Ok(records));
    }

    #[test]
    fn an_empty_v2_hint_file_decodes_to_nothing() {
        let file = with_trailer(crate::format::HINT_MAGIC.to_vec());
        assert_eq!(decode_hint_file(&file), Ok(Vec::new()));
    }

    #[test]
    fn a_v1_hint_file_decodes_with_every_seq_at_one() {
        let header = EntryHeader {
            timestamp: 4,
            key_size: 1,
            value_size: 2,
            tombstone: false,
        };
        let content = encode_hint(b"k", &header, 99).into_bytes();
        let got = decode_hint_file(&with_trailer(content)).unwrap();
        assert_eq!(
            got,
            vec![HintRecord {
                seq: 1,
                timestamp: 4,
                value_size: 2,
                tombstone: false,
                value_pos: 99,
                key: b"k".to_vec(),
            }]
        );
    }

    #[test]
    fn a_v2_hint_file_ending_inside_a_record_is_rejected() {
        let mut content = crate::format::HINT_MAGIC.to_vec();
        let record = encode_hint_v2(&v2_record(b"key", 1));
        content.extend_from_slice(&record.as_bytes()[..record.len() - 1]);
        assert!(decode_hint_file(&with_trailer(content)).is_err());
    }

    #[test]
    fn a_hint_file_failing_its_crc_is_rejected() {
        let mut file = with_trailer(crate::format::HINT_MAGIC.to_vec());
        file[0] ^= 1;
        assert!(decode_hint_file(&file).is_err());
    }
}
