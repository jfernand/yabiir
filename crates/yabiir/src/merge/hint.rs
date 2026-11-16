//! Writing a hint file.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use crate::format::{self, HintRecord};

/// Writes one version 2 hint file: the magic number, a record per entry of
/// the data file beside it, then a CRC32 of everything before it (the
/// layout is in `crate::format`).
pub(super) struct HintWriter {
    writer: BufWriter<File>,
    crc: crc32fast::Hasher,
}

impl HintWriter {
    pub(super) fn create(path: &Path) -> io::Result<Self> {
        let mut hint = Self {
            writer: BufWriter::new(File::create(path)?),
            crc: crc32fast::Hasher::new(),
        };
        hint.crc
            .update(&format::HINT_MAGIC);
        hint.writer
            .write_all(&format::HINT_MAGIC)?;
        Ok(hint)
    }

    /// Add `record`.
    pub(super) fn write(&mut self, record: &HintRecord) -> io::Result<()> {
        let record = format::encode_hint_v2(record);
        self.crc
            .update(record.as_bytes());
        self.writer
            .write_all(record.as_bytes())
    }

    /// Write the trailing CRC and flush. The file isn't fsynced: a hint file
    /// that didn't reach the disk whole fails its CRC at the next open, and
    /// recovery scans the data file instead, which was fsynced.
    pub(super) fn finish(mut self) -> io::Result<()> {
        let crc = self
            .crc
            .finalize();
        self.writer
            .write_all(&crc.to_le_bytes())?;
        self.writer
            .flush()
    }
}
