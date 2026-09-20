//! `.bitcask.hint` record encoding/decoding. See the parent module
//! (`crate::format`) for the full on-disk byte layout this implements.

use std::io::{self, Read};

use super::{EntryHeader, decode_key_size, encode_key_size, read_up_to};

pub const HINT_HEADER_SIZE: usize = 20;

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
        assert_eq!(encoded.as_ref().len(), expected_len);
    }
}
