//! The header that starts every data file written in format version 2, and
//! how a file of either version is told apart.
//!
//! A version 1 file (yabiir 0.8 and earlier) has no header: its first bytes
//! are its first entry's CRC. A version 2 file starts with an 8-byte magic
//! number, so a v1 file can only be mistaken for one if its first CRC and
//! timestamp happen to spell it, a chance of one in 2⁶⁴.

/// The first bytes of every version 2 data file.
pub const DATA_MAGIC: [u8; 8] = *b"ybcdat02";
/// The first bytes of every version 2 hint file.
pub const HINT_MAGIC: [u8; 8] = *b"ybchnt02";

/// Magic number, sequence floor, a CRC of both, and four reserved bytes.
pub const FILE_HEADER_SIZE: usize = 24;

/// The header for a new version 2 data file. `seq_floor` is the sequence
/// number the store would give its next write when the file was created:
/// every record written after that has a sequence number at least as high.
pub fn encode_file_header(seq_floor: u64) -> [u8; FILE_HEADER_SIZE] {
    let mut header = [0u8; FILE_HEADER_SIZE];
    header[..8].copy_from_slice(&DATA_MAGIC);
    header[8..16].copy_from_slice(&seq_floor.to_le_bytes());
    let crc = crc32fast::hash(&header[..16]);
    header[16..20].copy_from_slice(&crc.to_le_bytes());
    header
}

/// Which format a data file is in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileFormat {
    /// No header; entries from byte 0, each its own write.
    V1,
    /// A header, then records from [`FILE_HEADER_SIZE`].
    V2 {
        /// The header's sequence floor, or 0 if the header is torn or fails
        /// its CRC.
        seq_floor: u64,
    },
}

impl FileFormat {
    /// Where the first entry or record starts.
    pub fn data_start(self) -> u64 {
        match self {
            FileFormat::V1 => 0,
            FileFormat::V2 { .. } => FILE_HEADER_SIZE as u64,
        }
    }
}

/// Tell a data file's format from its first bytes: `prefix` holds the
/// first [`FILE_HEADER_SIZE`] bytes, or the whole file if it is shorter.
///
/// A file that is empty, or ends inside the magic number, is a version 2
/// file whose header was torn by a crash right after it was created: it
/// holds no records.
pub fn identify(prefix: &[u8]) -> FileFormat {
    let magic_len = prefix
        .len()
        .min(DATA_MAGIC.len());
    if prefix[..magic_len] != DATA_MAGIC[..magic_len] {
        return FileFormat::V1;
    }
    let seq_floor = match prefix.get(..FILE_HEADER_SIZE) {
        Some(header) if crc32fast::hash(&header[..16]).to_le_bytes() == header[16..20] => {
            u64::from_le_bytes(
                header[8..16]
                    .try_into()
                    .unwrap(),
            )
        }
        _ => 0,
    };
    FileFormat::V2 { seq_floor }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::encode_entry;

    #[test]
    fn a_header_identifies_as_v2_with_its_floor() {
        let header = encode_file_header(123_456);
        assert_eq!(identify(&header), FileFormat::V2 { seq_floor: 123_456 });
    }

    #[test]
    fn a_v1_file_identifies_as_v1() {
        let entry = encode_entry(b"key", b"value", false, 1_700_000_000);
        assert_eq!(
            identify(&entry.as_bytes()[..FILE_HEADER_SIZE]),
            FileFormat::V1
        );
        // Shorter than a header too: a v1 file holding one small entry.
        let small = encode_entry(b"k", b"", false, 0);
        assert!(small.len() < FILE_HEADER_SIZE);
        assert_eq!(identify(small.as_bytes()), FileFormat::V1);
    }

    #[test]
    fn an_empty_or_torn_header_is_an_empty_v2_file() {
        let header = encode_file_header(9);
        for cut in 0..FILE_HEADER_SIZE {
            assert_eq!(
                identify(&header[..cut]),
                FileFormat::V2 { seq_floor: 0 },
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn a_header_failing_its_crc_keeps_the_format_but_loses_the_floor() {
        let mut header = encode_file_header(9);
        header[10] ^= 1;
        assert_eq!(identify(&header), FileFormat::V2 { seq_floor: 0 });
    }

    #[test]
    fn data_starts_after_the_header_only_in_v2() {
        assert_eq!(FileFormat::V1.data_start(), 0);
        assert_eq!(
            FileFormat::V2 { seq_floor: 0 }.data_start(),
            FILE_HEADER_SIZE as u64
        );
    }
}
