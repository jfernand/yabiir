//! Startup recovery: rebuilding the keydir from on-disk data/hint files
//! when opening an existing Bitcask directory. See
//! `docs/bitcask-implementation-plan.md` §6.

use std::fs::File;
use std::io;
use std::path::Path;

use crate::datafile::DataFileSet;
use crate::format::{self, EntryRead, HintRead};
use crate::keydir::{Keydir, KeydirEntry};

/// Rebuild `keydir` from `file_ids` (as returned by [`DataFileSet::discover`],
/// ascending), preferring a file's hint file (fast path, no value bytes
/// read) when one exists, falling back to a full scan of the data file
/// otherwise.
///
/// Files are processed in ascending `file_id` order, and entries within a
/// file in ascending offset order; combined with [`apply_entry`]'s
/// unconditional overwrite-or-remove, that ordering is what makes "last
/// write wins" fall out for free with no special-casing — the single most
/// important invariant in this module (plan §6.2).
///
/// A `.bitcask.hint` file with no matching `.bitcask.data` file (orphaned —
/// e.g. left behind by an interrupted merge) is never visited at all, since
/// this only ever looks up a hint path for a `file_id` that `discover`
/// already found a data file for. That's the "ignore it" plan §6.5 asks
/// for, with no extra code needed to get it.
pub fn recover(dir: &Path, file_ids: &[u32], keydir: &mut Keydir) -> io::Result<()> {
    for (index, &file_id) in file_ids.iter().enumerate() {
        let is_last = index + 1 == file_ids.len();
        let hint_path = DataFileSet::hint_path(dir, file_id);
        if hint_path.exists() {
            scan_hint_file(&hint_path, file_id, keydir)?;
        } else {
            let data_path = DataFileSet::data_path(dir, file_id);
            scan_data_file(&data_path, file_id, is_last, keydir)?;
        }
    }
    Ok(())
}

fn apply_entry(keydir: &mut Keydir, file_id: u32, entry_start_pos: u64, entry: format::Entry) {
    let value_pos = entry_start_pos + format::HEADER_SIZE as u64 + entry.header.key_size as u64;
    if entry.header.tombstone {
        keydir.remove(&entry.key);
    } else {
        keydir.insert(
            &entry.key,
            KeydirEntry {
                file_id,
                value_size: entry.header.value_size,
                value_pos,
                timestamp: entry.header.timestamp,
            },
        );
    }
}

/// Full scan of one data file (no hint file available) — the slow path.
/// Stops cleanly at a truncated tail: expected on the most-recently-active
/// file after an unclean shutdown (`is_last`, no warning), but a warning on
/// any earlier, already-rotated file, since a truncated *closed* file
/// indicates real corruption rather than a torn write. A single
/// complete-but-corrupt entry (CRC mismatch — bitrot) is skipped, with a
/// warning, without aborting the rest of the scan.
fn scan_data_file(path: &Path, file_id: u32, is_last: bool, keydir: &mut Keydir) -> io::Result<()> {
    let mut f = File::open(path)?;
    let mut pos: u64 = 0;
    loop {
        match format::read_entry(&mut f)? {
            None => break, // clean EOF exactly at an entry boundary — done
            Some(EntryRead::Truncated) => {
                if !is_last {
                    eprintln!(
                        "warning: {} has a truncated entry at offset {pos} but is not \
                         the most recently active file — possible corruption",
                        path.display()
                    );
                }
                break;
            }
            Some(EntryRead::Ok(entry, total_len)) => {
                apply_entry(keydir, file_id, pos, entry);
                pos += total_len;
            }
            Some(EntryRead::CrcMismatch { total_len }) => {
                eprintln!(
                    "warning: {} has a corrupt entry at offset {pos} (CRC mismatch), skipping it",
                    path.display()
                );
                pos += total_len;
            }
        }
    }
    Ok(())
}

/// Fast-path recovery from a hint file: no value bytes to read, and
/// `value_pos` is carried directly in each record rather than derived from
/// a running offset.
fn scan_hint_file(path: &Path, file_id: u32, keydir: &mut Keydir) -> io::Result<()> {
    let mut f = File::open(path)?;
    loop {
        match format::read_hint(&mut f)? {
            None => break,
            Some(HintRead::Truncated) => {
                // Shouldn't happen for a hint file written by a completed
                // merge, but handle it the same way as a truncated data
                // file tail: stop, keep what was already recovered.
                eprintln!(
                    "warning: {} has a truncated hint record, stopping scan there",
                    path.display()
                );
                break;
            }
            Some(HintRead::Ok {
                header,
                value_pos,
                key,
            }) => {
                if header.tombstone {
                    // Merge never currently writes tombstones to hint files
                    // (it drops dead entries during compaction instead), so
                    // this branch is dead code today — kept for format
                    // symmetry with scan_data_file and for future-proofing.
                    keydir.remove(&key);
                } else {
                    keydir.insert(
                        &key,
                        KeydirEntry {
                            file_id,
                            value_size: header.value_size,
                            value_pos,
                            timestamp: header.timestamp,
                        },
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datafile::ActiveFile;
    use std::fs::{self, OpenOptions};
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Minimal self-cleaning temp directory — same pattern used throughout
    /// this crate's tests.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "yabiir-recovery-test-{}-{}-{}",
                std::process::id(),
                n,
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl std::ops::Deref for TempDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Write one entry into `file_id` and return `(value_pos, total_len,
    /// EntryHeader)`, for tests that need to hand-construct hint files or
    /// compute byte offsets to truncate/corrupt.
    fn write_entry(
        dir: &Path,
        active: &mut ActiveFile,
        key: &[u8],
        value: &[u8],
        tombstone: bool,
        tstamp: u32,
    ) -> (u64, u64, format::EntryHeader) {
        let _ = dir;
        let encoded = format::encode_entry(key, value, tombstone, tstamp);
        let header = format::EntryHeader {
            timestamp: tstamp,
            key_size: key.len() as u32,
            value_size: value.len() as u32,
            tombstone,
        };
        let (_, value_pos, total_len) = active.append(&encoded).unwrap();
        (value_pos, total_len, header)
    }

    fn read_value(dir: &Path, entry: KeydirEntry) -> Vec<u8> {
        DataFileSet::new(dir)
            .read_at(entry.file_id, entry.value_pos, entry.value_size)
            .unwrap()
    }

    #[test]
    fn full_scan_recovery_is_last_write_wins() {
        let dir = TempDir::new();

        let mut active0 = ActiveFile::create(&dir, 0).unwrap();
        write_entry(&dir, &mut active0, b"a", b"a-v1", false, 1);
        write_entry(&dir, &mut active0, b"b", b"b-v1", false, 2);
        active0.sync().unwrap();
        drop(active0);

        let mut active1 = ActiveFile::create(&dir, 1).unwrap();
        write_entry(&dir, &mut active1, b"a", b"a-v2", false, 3); // overwrites file 0's "a"
        write_entry(&dir, &mut active1, b"c", b"c-v1", false, 4);
        active1.sync().unwrap();
        drop(active1);

        let file_ids = DataFileSet::discover(&dir).unwrap();
        assert_eq!(file_ids, vec![0, 1]);

        let mut keydir = Keydir::new();
        recover(&dir, &file_ids, &mut keydir).unwrap();

        assert_eq!(read_value(&dir, keydir.get(b"a").unwrap()), b"a-v2");
        assert_eq!(read_value(&dir, keydir.get(b"b").unwrap()), b"b-v1");
        assert_eq!(read_value(&dir, keydir.get(b"c").unwrap()), b"c-v1");
        assert_eq!(keydir.len(), 3);
    }

    #[test]
    fn hint_based_recovery_matches_full_scan() {
        // Build the exact same two-file layout as the full-scan test above,
        // but this time also write a hint file for file 0, and assert the
        // recovered keydir is equivalent (same live values for every key).
        let dir = TempDir::new();

        let mut active0 = ActiveFile::create(&dir, 0).unwrap();
        let (a_pos, _, a_header) = write_entry(&dir, &mut active0, b"a", b"a-v1", false, 1);
        let (b_pos, _, b_header) = write_entry(&dir, &mut active0, b"b", b"b-v1", false, 2);
        active0.sync().unwrap();
        drop(active0);

        // Hand-write file 0's hint file describing exactly what's in it.
        let mut hint = File::create(DataFileSet::hint_path(&dir, 0)).unwrap();
        hint.write_all(format::encode_hint(b"a", &a_header, a_pos).as_bytes())
            .unwrap();
        hint.write_all(format::encode_hint(b"b", &b_header, b_pos).as_bytes())
            .unwrap();
        drop(hint);

        let mut active1 = ActiveFile::create(&dir, 1).unwrap();
        write_entry(&dir, &mut active1, b"a", b"a-v2", false, 3);
        write_entry(&dir, &mut active1, b"c", b"c-v1", false, 4);
        active1.sync().unwrap();
        drop(active1);

        let file_ids = DataFileSet::discover(&dir).unwrap();
        let mut keydir = Keydir::new();
        recover(&dir, &file_ids, &mut keydir).unwrap();

        assert_eq!(read_value(&dir, keydir.get(b"a").unwrap()), b"a-v2");
        assert_eq!(read_value(&dir, keydir.get(b"b").unwrap()), b"b-v1");
        assert_eq!(read_value(&dir, keydir.get(b"c").unwrap()), b"c-v1");
        assert_eq!(keydir.len(), 3);
    }

    #[test]
    fn truncated_tail_keeps_earlier_entries_and_does_not_error() {
        let dir = TempDir::new();
        let mut active = ActiveFile::create(&dir, 0).unwrap();
        write_entry(&dir, &mut active, b"a", b"a-v1", false, 1);
        write_entry(&dir, &mut active, b"b", b"b-v1", false, 2);
        let (_, last_total_len, _) = write_entry(&dir, &mut active, b"c", b"c-v1", false, 3);
        active.sync().unwrap();
        drop(active);

        // Truncate a few bytes into the last entry's value.
        let path = DataFileSet::data_path(&dir, 0);
        let full_len = fs::metadata(&path).unwrap().len();
        let new_len = full_len - (last_total_len.min(3));
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(new_len)
            .unwrap();

        let file_ids = DataFileSet::discover(&dir).unwrap();
        let mut keydir = Keydir::new();
        recover(&dir, &file_ids, &mut keydir).unwrap(); // must not error

        assert_eq!(read_value(&dir, keydir.get(b"a").unwrap()), b"a-v1");
        assert_eq!(read_value(&dir, keydir.get(b"b").unwrap()), b"b-v1");
        assert_eq!(keydir.get(b"c"), None); // torn write, correctly dropped
    }

    #[test]
    fn corrupt_entry_mid_file_is_skipped_but_scan_continues() {
        let dir = TempDir::new();
        let mut active = ActiveFile::create(&dir, 0).unwrap();
        write_entry(&dir, &mut active, b"a", b"a-v1", false, 1);
        let (b_pos, _, _) = write_entry(&dir, &mut active, b"b", b"b-v1", false, 2);
        write_entry(&dir, &mut active, b"c", b"c-v1", false, 3);
        active.sync().unwrap();
        drop(active);

        // Flip a bit inside "b"'s value bytes — CRC will no longer match.
        let path = DataFileSet::data_path(&dir, 0);
        let mut f = OpenOptions::new().read(true).write(true).open(&path).unwrap();
        f.seek(SeekFrom::Start(b_pos)).unwrap();
        let mut byte = [0u8; 1];
        f.read_exact(&mut byte).unwrap();
        byte[0] ^= 0xFF;
        f.seek(SeekFrom::Start(b_pos)).unwrap();
        f.write_all(&byte).unwrap();
        drop(f);

        let file_ids = DataFileSet::discover(&dir).unwrap();
        let mut keydir = Keydir::new();
        recover(&dir, &file_ids, &mut keydir).unwrap();

        assert_eq!(read_value(&dir, keydir.get(b"a").unwrap()), b"a-v1");
        assert_eq!(keydir.get(b"b"), None); // corrupt entry skipped, not applied
        assert_eq!(read_value(&dir, keydir.get(b"c").unwrap()), b"c-v1");
    }

    #[test]
    fn orphaned_hint_file_is_ignored() {
        let dir = TempDir::new();
        // A hint file with no matching data file.
        File::create(DataFileSet::hint_path(&dir, 0)).unwrap();

        let file_ids = DataFileSet::discover(&dir).unwrap();
        assert!(file_ids.is_empty()); // discover only ever lists *.bitcask.data

        let mut keydir = Keydir::new();
        recover(&dir, &file_ids, &mut keydir).unwrap(); // must not error
        assert_eq!(keydir.len(), 0);
    }

    #[test]
    fn truncation_on_a_non_last_file_still_recovers_what_it_can() {
        // Two files; truncate the OLDER (non-last) one. Recovery should
        // still keep everything decodable before the truncation point, log
        // a warning (not asserted here — stderr), and not error.
        let dir = TempDir::new();
        let mut active0 = ActiveFile::create(&dir, 0).unwrap();
        write_entry(&dir, &mut active0, b"a", b"a-v1", false, 1);
        let (_, last_total_len, _) = write_entry(&dir, &mut active0, b"b", b"b-v1", false, 2);
        active0.sync().unwrap();
        drop(active0);

        let path0 = DataFileSet::data_path(&dir, 0);
        let full_len = fs::metadata(&path0).unwrap().len();
        OpenOptions::new()
            .write(true)
            .open(&path0)
            .unwrap()
            .set_len(full_len - last_total_len.min(3))
            .unwrap();

        let mut active1 = ActiveFile::create(&dir, 1).unwrap();
        write_entry(&dir, &mut active1, b"c", b"c-v1", false, 3);
        active1.sync().unwrap();
        drop(active1);

        let file_ids = DataFileSet::discover(&dir).unwrap();
        let mut keydir = Keydir::new();
        recover(&dir, &file_ids, &mut keydir).unwrap();

        assert_eq!(read_value(&dir, keydir.get(b"a").unwrap()), b"a-v1");
        assert_eq!(keydir.get(b"b"), None);
        assert_eq!(read_value(&dir, keydir.get(b"c").unwrap()), b"c-v1");
    }
}
