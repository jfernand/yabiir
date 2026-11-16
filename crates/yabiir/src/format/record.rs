//! Version 2 data-file records: one or more operations written as one unit,
//! under one CRC and one sequence number. See the parent module
//! (`crate::format`) for the byte layout.
//!
//! A record is what makes a write batch atomic. Recovery reads a record
//! whole or not at all: a crash in the middle of appending one leaves a
//! truncated record, and bit rot anywhere in one fails its CRC, and in both
//! cases none of its operations is applied. A single `put` or `delete` is a
//! record with one operation.

use std::io::{self, Read};

use super::{decode_key_size, encode_key_size, read_up_to};

/// The fixed part of a record: CRC, body length, sequence number, timestamp
/// and operation count.
pub const RECORD_HEADER_SIZE: usize = 24;
/// The fixed part of each operation in a record's body: key length (with
/// the tombstone flag) and value length.
pub const OP_HEADER_SIZE: usize = 8;

const CRC_LEN: usize = 4;

/// One operation to encode: a put of `value` under `key`, or, with
/// `tombstone`, a delete of `key` (whose `value` is empty).
#[derive(Clone, Copy, Debug)]
pub struct OpRef<'a> {
    pub key: &'a [u8],
    pub value: &'a [u8],
    pub tombstone: bool,
}

/// An encoded record, ready to be appended to a data file, and where each
/// operation's value sits within it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedRecord {
    bytes: Vec<u8>,
    /// For each operation, the offset of its value from the start of the
    /// record. The keydir stores the value's position in the file, which is
    /// the record's position plus this.
    value_offsets: Vec<u64>,
}

impl EncodedRecord {
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

    /// Where operation `i`'s value starts, counted from the record's start.
    pub fn value_offset(&self, i: usize) -> u64 {
        self.value_offsets[i]
    }
}

/// Encode `ops` as one record with sequence number `seq` and timestamp
/// `tstamp`.
///
/// The caller checks the sizes first: every key must fit in
/// [`MAX_KEY_LEN`](super::MAX_KEY_LEN) bytes and every value in
/// [`MAX_VALUE_LEN`](super::MAX_VALUE_LEN), and the body in total in
/// `u32::MAX` bytes ([`body_len`] says how long it is). `Engine` does.
pub fn encode_record(seq: u64, tstamp: u32, ops: &[OpRef<'_>]) -> EncodedRecord {
    let body_len = body_len(ops);
    debug_assert!(body_len <= u32::MAX as u64, "record body too long");
    let mut bytes = Vec::with_capacity(RECORD_HEADER_SIZE + body_len as usize);
    bytes.extend_from_slice(&[0u8; CRC_LEN]); // filled in below
    bytes.extend_from_slice(&(body_len as u32).to_le_bytes());
    bytes.extend_from_slice(&seq.to_le_bytes());
    bytes.extend_from_slice(&tstamp.to_le_bytes());
    bytes.extend_from_slice(&(ops.len() as u32).to_le_bytes());

    let mut value_offsets = Vec::with_capacity(ops.len());
    for op in ops {
        debug_assert!(
            !op.tombstone
                || op
                    .value
                    .is_empty()
        );
        let ksz_and_flags = encode_key_size(
            op.key
                .len() as u32,
            op.tombstone,
        );
        bytes.extend_from_slice(&ksz_and_flags.to_le_bytes());
        bytes.extend_from_slice(
            &(op.value
                .len() as u32)
                .to_le_bytes(),
        );
        bytes.extend_from_slice(op.key);
        value_offsets.push(bytes.len() as u64);
        bytes.extend_from_slice(op.value);
    }

    let crc = crc32fast::hash(&bytes[CRC_LEN..]);
    bytes[..CRC_LEN].copy_from_slice(&crc.to_le_bytes());
    EncodedRecord {
        bytes,
        value_offsets,
    }
}

/// How many bytes `ops` take in a record's body.
pub fn body_len(ops: &[OpRef<'_>]) -> u64 {
    ops.iter()
        .map(|op| {
            OP_HEADER_SIZE as u64
                + op.key
                    .len() as u64
                + op.value
                    .len() as u64
        })
        .sum()
}

/// A decoded record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Record {
    pub seq: u64,
    pub timestamp: u32,
    pub ops: Vec<RecordOp>,
}

/// One decoded operation of a [`Record`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordOp {
    pub key: Vec<u8>,
    /// Empty for a tombstone.
    pub value: Vec<u8>,
    pub tombstone: bool,
    /// Where the value starts, counted from the start of the record.
    pub value_offset: u64,
}

/// What [`read_record`] found.
#[derive(Debug)]
pub enum RecordRead {
    /// A whole record with a valid CRC, and its length on disk.
    Ok(Record, u64),
    /// The source ends inside the record: a torn write.
    Truncated,
    /// A whole record whose CRC doesn't match, or whose body doesn't parse
    /// into the operations its header announces. Carries its length on
    /// disk, so a scan can skip it.
    Corrupt { total_len: u64 },
}

/// Read one record from `r`, positioned at its start. `available` is how
/// many bytes `r` holds from there: a header claiming a longer body is a
/// torn write, found before anything is allocated for it. Returns `Ok(None)`
/// at a clean end of the source.
pub fn read_record<R: Read>(r: &mut R, available: u64) -> io::Result<Option<RecordRead>> {
    let mut header = [0u8; RECORD_HEADER_SIZE];
    let n = read_up_to(r, &mut header)?;
    if n == 0 {
        return Ok(None);
    }
    if n < RECORD_HEADER_SIZE {
        return Ok(Some(RecordRead::Truncated));
    }
    let crc = u32_at(&header, 0);
    let body_len = u32_at(&header, 4) as u64;
    let total_len = RECORD_HEADER_SIZE as u64 + body_len;
    if total_len > available {
        return Ok(Some(RecordRead::Truncated));
    }

    // Everything the CRC covers in one buffer, hashed in one call (see
    // `read_entry` for why).
    let mut covered = vec![0u8; RECORD_HEADER_SIZE - CRC_LEN + body_len as usize];
    covered[..RECORD_HEADER_SIZE - CRC_LEN].copy_from_slice(&header[CRC_LEN..]);
    let n = read_up_to(r, &mut covered[RECORD_HEADER_SIZE - CRC_LEN..])?;
    if (n as u64) < body_len {
        return Ok(Some(RecordRead::Truncated));
    }
    if crc32fast::hash(&covered) != crc {
        return Ok(Some(RecordRead::Corrupt { total_len }));
    }

    let seq = u64::from_le_bytes(
        header[8..16]
            .try_into()
            .unwrap(),
    );
    let timestamp = u32_at(&header, 16);
    let n_ops = u32_at(&header, 20);
    match parse_ops(&covered[RECORD_HEADER_SIZE - CRC_LEN..], n_ops) {
        Some(ops) => Ok(Some(RecordRead::Ok(
            Record {
                seq,
                timestamp,
                ops,
            },
            total_len,
        ))),
        // Only a writer that encoded a wrong body could get here, since the
        // CRC matched.
        None => Ok(Some(RecordRead::Corrupt { total_len })),
    }
}

/// Split a record's body into its `n_ops` operations, or `None` if it
/// doesn't hold exactly that many.
fn parse_ops(body: &[u8], n_ops: u32) -> Option<Vec<RecordOp>> {
    let mut ops = Vec::with_capacity((n_ops as usize).min(body.len() / OP_HEADER_SIZE + 1));
    let mut at = 0usize;
    for _ in 0..n_ops {
        let op_header = body.get(at..at + OP_HEADER_SIZE)?;
        let (key_len, tombstone) = decode_key_size(u32_at(op_header, 0));
        let value_len = u32_at(op_header, 4) as usize;
        let key_start = at + OP_HEADER_SIZE;
        let value_start = key_start.checked_add(key_len as usize)?;
        let value_end = value_start.checked_add(value_len)?;
        let key = body.get(key_start..value_start)?;
        let value = body.get(value_start..value_end)?;
        ops.push(RecordOp {
            key: key.to_vec(),
            value: value.to_vec(),
            tombstone,
            value_offset: (RECORD_HEADER_SIZE + value_start) as u64,
        });
        at = value_end;
    }
    (at == body.len()).then_some(ops)
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(
        bytes[at..at + 4]
            .try_into()
            .unwrap(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn put<'a>(key: &'a [u8], value: &'a [u8]) -> OpRef<'a> {
        OpRef {
            key,
            value,
            tombstone: false,
        }
    }

    fn delete(key: &[u8]) -> OpRef<'_> {
        OpRef {
            key,
            value: &[],
            tombstone: true,
        }
    }

    fn read_all(bytes: &[u8]) -> Option<RecordRead> {
        read_record(&mut Cursor::new(bytes), bytes.len() as u64).unwrap()
    }

    #[test]
    fn a_record_round_trips_with_every_operation_in_order() {
        let big = vec![7u8; 100_000];
        let ops = [
            put(b"a", b"1"),
            delete(b"b"),
            put(b"c", b""),
            put(b"d", &big),
        ];
        let encoded = encode_record(42, 1_700_000_000, &ops);
        let Some(RecordRead::Ok(record, total_len)) = read_all(encoded.as_bytes()) else {
            panic!("expected a record");
        };
        assert_eq!(total_len, encoded.len() as u64);
        assert_eq!(record.seq, 42);
        assert_eq!(record.timestamp, 1_700_000_000);
        assert_eq!(
            record
                .ops
                .len(),
            ops.len()
        );
        for (i, (got, want)) in record
            .ops
            .iter()
            .zip(&ops)
            .enumerate()
        {
            assert_eq!(got.key, want.key);
            assert_eq!(got.value, want.value);
            assert_eq!(got.tombstone, want.tombstone);
            assert_eq!(got.value_offset, encoded.value_offset(i));
            let at = got.value_offset as usize;
            assert_eq!(
                &encoded.as_bytes()[at..at
                    + want
                        .value
                        .len()],
                want.value
            );
        }
    }

    #[test]
    fn an_empty_record_round_trips() {
        let encoded = encode_record(1, 0, &[]);
        assert_eq!(encoded.len(), RECORD_HEADER_SIZE);
        let Some(RecordRead::Ok(record, _)) = read_all(encoded.as_bytes()) else {
            panic!("expected a record");
        };
        assert!(
            record
                .ops
                .is_empty()
        );
    }

    #[test]
    fn a_record_cut_anywhere_reads_as_truncated() {
        let encoded = encode_record(9, 0, &[put(b"key", b"value"), delete(b"gone")]);
        let bytes = encoded.as_bytes();
        for cut in 1..bytes.len() {
            let got = read_record(&mut Cursor::new(&bytes[..cut]), cut as u64).unwrap();
            assert!(
                matches!(got, Some(RecordRead::Truncated)),
                "cut at {cut}: {got:?}"
            );
        }
    }

    #[test]
    fn a_flipped_bit_anywhere_after_the_length_reads_as_corrupt() {
        let encoded = encode_record(9, 3, &[put(b"key", b"value"), delete(b"gone")]);
        // The length field (bytes 4..8) decides how much is read, so a flip
        // there reads as truncated or as a different record; past it, every
        // flip has to fail the CRC.
        for at in (0..4).chain(8..encoded.len()) {
            let mut bytes = encoded
                .as_bytes()
                .to_vec();
            bytes[at] ^= 0x10;
            let got = read_all(&bytes);
            assert!(
                matches!(got, Some(RecordRead::Corrupt { total_len }) if total_len == bytes.len() as u64),
                "flip at {at}: {got:?}"
            );
        }
    }

    #[test]
    fn a_length_longer_than_the_source_is_truncated_without_reading_the_body() {
        let mut bytes = encode_record(1, 0, &[put(b"k", b"v")])
            .as_bytes()
            .to_vec();
        bytes[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        let available = bytes.len() as u64;
        let got = read_record(&mut Cursor::new(bytes), available).unwrap();
        assert!(matches!(got, Some(RecordRead::Truncated)), "{got:?}");
    }

    #[test]
    fn a_body_that_disagrees_with_its_op_count_is_corrupt_even_with_a_valid_crc() {
        let mut bytes = encode_record(1, 0, &[put(b"k", b"v")])
            .as_bytes()
            .to_vec();
        bytes[20..24].copy_from_slice(&2u32.to_le_bytes());
        let crc = crc32fast::hash(&bytes[CRC_LEN..]);
        bytes[..CRC_LEN].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(read_all(&bytes), Some(RecordRead::Corrupt { .. })));
    }

    #[test]
    fn a_clean_end_reads_as_none() {
        assert!(read_all(&[]).is_none());
    }

    #[test]
    fn body_len_counts_every_op_header_key_and_value() {
        let ops = [put(b"ab", b"cde"), delete(b"f")];
        assert_eq!(body_len(&ops), (OP_HEADER_SIZE * 2 + 2 + 3 + 1) as u64);
        assert_eq!(
            encode_record(0, 0, &ops).len() as u64,
            RECORD_HEADER_SIZE as u64 + body_len(&ops)
        );
    }
}
