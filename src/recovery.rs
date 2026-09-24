//! Startup recovery: rebuilding the keydir from on-disk data/hint files
//! when opening an existing Bitcask directory. See
//! `docs/bitcask-implementation-plan.md` §6.

use std::fs::{self, File};
use std::io;
use std::path::Path;

use crate::datafile::DataFileSet;
use crate::format::{self, EntryRead, HintRead};
use crate::keydir::{Keydir, KeydirEntry};

/// Rebuild `keydir` from `file_ids` (as returned by [`DataFileSet::discover`],
/// ascending), preferring a file's hint file (fast path, no value bytes
/// read) when one exists *and* passes validation, falling back to a full
/// scan of the data file otherwise — a missing hint file, a failed
/// whole-file CRC check (`format::verify_hint_file`), or a record whose
/// pointer doesn't fit inside the data file all count as "doesn't pass
/// validation". A hint file is only ever trusted as a whole: nothing from
/// an invalid one is applied to `keydir` before falling back, matching
/// upstream Bitcask's own behavior (`bitcask_fileops.erl`'s `fold_keys`).
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
    for (index, &file_id) in file_ids
        .iter()
        .enumerate()
    {
        let is_last = index + 1 == file_ids.len();
        let data_path = DataFileSet::data_path(dir, file_id);
        let hint_path = DataFileSet::hint_path(dir, file_id);
        let used_hint =
            hint_path.exists() && try_scan_hint_file(&hint_path, file_id, &data_path, keydir)?;
        if !used_hint {
            scan_data_file(&data_path, file_id, is_last, keydir)?;
        }
    }
    Ok(())
}

fn apply_entry(keydir: &mut Keydir, file_id: u32, entry_start_pos: u64, entry: format::Entry) {
    let value_pos = entry_start_pos
        + format::HEADER_SIZE as u64
        + entry
            .header
            .key_size as u64;
    if entry
        .header
        .tombstone
    {
        keydir.remove(&entry.key);
    } else {
        keydir.insert(
            &entry.key,
            KeydirEntry {
                file_id,
                value_size: entry
                    .header
                    .value_size,
                value_pos,
                timestamp: entry
                    .header
                    .timestamp,
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
                    crate::log::warn!(
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
                crate::log::warn!(
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
/// a running offset. Returns `true` if the hint file passed validation and
/// was fully applied to `keydir`; `false` if it failed its whole-file CRC,
/// contained a truncated record, or pointed outside the data file — in any
/// of those cases `keydir` is left untouched by this call, and the caller
/// falls back to [`scan_data_file`] instead. Every record is validated
/// *before* any of them are applied, so a bad hint file never partially
/// pollutes the keydir before the fallback runs.
fn try_scan_hint_file(
    hint_path: &Path,
    file_id: u32,
    data_path: &Path,
    keydir: &mut Keydir,
) -> io::Result<bool> {
    let bytes = fs::read(hint_path)?;
    let Some(content) = format::verify_hint_file(&bytes) else {
        crate::log::warn!(
            "warning: {} failed its whole-file CRC check, falling back to a full scan \
             of the data file",
            hint_path.display()
        );
        return Ok(false);
    };

    let data_file_len = fs::metadata(data_path)?.len();
    let mut records = Vec::new();
    let mut cursor = io::Cursor::new(content);
    loop {
        match format::read_hint(&mut cursor)? {
            None => break,
            Some(HintRead::Truncated) => {
                // A CRC-valid file should never actually hit this (the
                // trailer covers every byte the writer produced), but stay
                // defensive rather than trust that invariant blindly.
                crate::log::warn!(
                    "warning: {} has a truncated hint record despite passing its CRC \
                     check, falling back to a full scan of the data file",
                    hint_path.display()
                );
                return Ok(false);
            }
            Some(HintRead::Ok {
                header,
                value_pos,
                key,
            }) => {
                if value_pos.saturating_add(header.value_size as u64) > data_file_len {
                    crate::log::warn!(
                        "warning: {} has an out-of-bounds pointer (value_pos {value_pos} + \
                         value_size {} > data file length {data_file_len}), falling back to \
                         a full scan of the data file",
                        hint_path.display(),
                        header.value_size
                    );
                    return Ok(false);
                }
                records.push((header, value_pos, key));
            }
        }
    }

    for (header, value_pos, key) in records {
        if header.tombstone {
            // Merge never currently writes tombstones to hint files (it
            // drops dead entries during compaction instead), so this
            // branch is dead code today — kept for format symmetry with
            // scan_data_file and for future-proofing.
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
    Ok(true)
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
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
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
        let (_, value_pos, total_len) = active
            .append(&encoded)
            .unwrap();
        (value_pos, total_len, header)
    }

    fn read_value(dir: &Path, entry: KeydirEntry) -> Vec<u8> {
        DataFileSet::new(dir)
            .read_at(entry.file_id, entry.value_pos, entry.value_size)
            .unwrap()
    }

    /// Write a well-formed hint file (records + a correct trailing CRC) for
    /// `file_id`, so a test can drive the hint-based fast path directly
    /// instead of the full-scan fallback.
    fn write_valid_hint_file(
        dir: &Path,
        file_id: u32,
        records: &[(&[u8], u64, format::EntryHeader)],
    ) {
        let mut content = Vec::new();
        for (key, value_pos, header) in records {
            content.extend_from_slice(format::encode_hint(key, header, *value_pos).as_bytes());
        }
        let trailer = crc32fast::hash(&content).to_le_bytes();
        let mut file = File::create(DataFileSet::hint_path(dir, file_id)).unwrap();
        file.write_all(&content)
            .unwrap();
        file.write_all(&trailer)
            .unwrap();
    }

    #[test]
    fn full_scan_recovery_is_last_write_wins() {
        let dir = TempDir::new();

        let mut active0 = ActiveFile::create(&dir, 0).unwrap();
        write_entry(&dir, &mut active0, b"a", b"a-v1", false, 1);
        write_entry(&dir, &mut active0, b"b", b"b-v1", false, 2);
        active0
            .sync()
            .unwrap();
        drop(active0);

        let mut active1 = ActiveFile::create(&dir, 1).unwrap();
        write_entry(&dir, &mut active1, b"a", b"a-v2", false, 3); // overwrites file 0's "a"
        write_entry(&dir, &mut active1, b"c", b"c-v1", false, 4);
        active1
            .sync()
            .unwrap();
        drop(active1);

        let file_ids = DataFileSet::discover(&dir).unwrap();
        assert_eq!(file_ids, vec![0, 1]);

        let mut keydir = Keydir::new();
        recover(&dir, &file_ids, &mut keydir).unwrap();

        assert_eq!(
            read_value(
                &dir,
                keydir
                    .get(b"a")
                    .unwrap()
            ),
            b"a-v2"
        );
        assert_eq!(
            read_value(
                &dir,
                keydir
                    .get(b"b")
                    .unwrap()
            ),
            b"b-v1"
        );
        assert_eq!(
            read_value(
                &dir,
                keydir
                    .get(b"c")
                    .unwrap()
            ),
            b"c-v1"
        );
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
        active0
            .sync()
            .unwrap();
        drop(active0);

        // Hand-write file 0's hint file describing exactly what's in it,
        // with a valid trailer so this actually exercises the hint-based
        // fast path (see try_scan_hint_file), not the fallback.
        write_valid_hint_file(&dir, 0, &[(b"a", a_pos, a_header), (b"b", b_pos, b_header)]);

        let mut active1 = ActiveFile::create(&dir, 1).unwrap();
        write_entry(&dir, &mut active1, b"a", b"a-v2", false, 3);
        write_entry(&dir, &mut active1, b"c", b"c-v1", false, 4);
        active1
            .sync()
            .unwrap();
        drop(active1);

        let file_ids = DataFileSet::discover(&dir).unwrap();
        let mut keydir = Keydir::new();
        recover(&dir, &file_ids, &mut keydir).unwrap();

        assert_eq!(
            read_value(
                &dir,
                keydir
                    .get(b"a")
                    .unwrap()
            ),
            b"a-v2"
        );
        assert_eq!(
            read_value(
                &dir,
                keydir
                    .get(b"b")
                    .unwrap()
            ),
            b"b-v1"
        );
        assert_eq!(
            read_value(
                &dir,
                keydir
                    .get(b"c")
                    .unwrap()
            ),
            b"c-v1"
        );
        assert_eq!(keydir.len(), 3);
    }

    /// A hint file that fails its whole-file CRC check must not be trusted
    /// at all — recovery falls back to a full scan of the data file for
    /// that file_id and still gets the exactly correct result, not a
    /// partially-applied mix of bad hint data and real data.
    #[test]
    fn hint_file_with_invalid_crc_falls_back_to_full_scan() {
        let dir = TempDir::new();
        let mut active = ActiveFile::create(&dir, 0).unwrap();
        write_entry(&dir, &mut active, b"a", b"a-v1", false, 1);
        write_entry(&dir, &mut active, b"b", b"b-v1", false, 2);
        active
            .sync()
            .unwrap();
        drop(active);

        // A hint file whose content doesn't match its own trailer at all —
        // and, worse, describes a key ("x") that was never really written,
        // at a bogus offset. If this were trusted, recovery would produce
        // a wrong keydir; falling back must ignore it completely.
        let bogus_header = format::EntryHeader {
            timestamp: 99,
            key_size: 1,
            value_size: 4,
            tombstone: false,
        };
        let mut file = File::create(DataFileSet::hint_path(&dir, 0)).unwrap();
        file.write_all(format::encode_hint(b"x", &bogus_header, 0).as_bytes())
            .unwrap();
        file.write_all(&0u32.to_le_bytes()) // wrong trailer, not this content's real CRC
            .unwrap();
        drop(file);

        let file_ids = DataFileSet::discover(&dir).unwrap();
        let mut keydir = Keydir::new();
        recover(&dir, &file_ids, &mut keydir).unwrap();

        assert_eq!(
            keydir.get(b"x"),
            None,
            "bogus hint-only key must not appear"
        );
        assert_eq!(
            read_value(
                &dir,
                keydir
                    .get(b"a")
                    .unwrap()
            ),
            b"a-v1"
        );
        assert_eq!(
            read_value(
                &dir,
                keydir
                    .get(b"b")
                    .unwrap()
            ),
            b"b-v1"
        );
        assert_eq!(keydir.len(), 2);
    }

    /// A hint file that passes its CRC (so it wasn't corrupted or torn) but
    /// contains a pointer past the end of the data file — internally
    /// inconsistent in a way a checksum alone can't catch (e.g. a hint file
    /// misattributed to the wrong data file) — must also fall back rather
    /// than hand out a `KeydirEntry` a read would fail (or worse, silently
    /// misread neighboring bytes) on.
    #[test]
    fn hint_file_with_out_of_bounds_pointer_falls_back_to_full_scan() {
        let dir = TempDir::new();
        let mut active = ActiveFile::create(&dir, 0).unwrap();
        let (a_pos, _, a_header) = write_entry(&dir, &mut active, b"a", b"a-v1", false, 1);
        active
            .sync()
            .unwrap();
        drop(active);

        let data_len = fs::metadata(DataFileSet::data_path(&dir, 0))
            .unwrap()
            .len();
        let out_of_bounds_header = format::EntryHeader {
            timestamp: 1,
            key_size: 1,
            value_size: 4,
            tombstone: false,
        };
        // A pointer that starts exactly at EOF — value_size alone already
        // pushes it past the file's real length.
        write_valid_hint_file(
            &dir,
            0,
            &[
                (b"a", a_pos, a_header),
                (b"z", data_len, out_of_bounds_header),
            ],
        );

        let file_ids = DataFileSet::discover(&dir).unwrap();
        let mut keydir = Keydir::new();
        recover(&dir, &file_ids, &mut keydir).unwrap();

        assert_eq!(
            keydir.get(b"z"),
            None,
            "out-of-bounds hint key must not appear"
        );
        assert_eq!(
            read_value(
                &dir,
                keydir
                    .get(b"a")
                    .unwrap()
            ),
            b"a-v1"
        );
        assert_eq!(keydir.len(), 1);
    }

    #[test]
    fn truncated_tail_keeps_earlier_entries_and_does_not_error() {
        let dir = TempDir::new();
        let mut active = ActiveFile::create(&dir, 0).unwrap();
        write_entry(&dir, &mut active, b"a", b"a-v1", false, 1);
        write_entry(&dir, &mut active, b"b", b"b-v1", false, 2);
        let (_, last_total_len, _) = write_entry(&dir, &mut active, b"c", b"c-v1", false, 3);
        active
            .sync()
            .unwrap();
        drop(active);

        // Truncate a few bytes into the last entry's value.
        let path = DataFileSet::data_path(&dir, 0);
        let full_len = fs::metadata(&path)
            .unwrap()
            .len();
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

        assert_eq!(
            read_value(
                &dir,
                keydir
                    .get(b"a")
                    .unwrap()
            ),
            b"a-v1"
        );
        assert_eq!(
            read_value(
                &dir,
                keydir
                    .get(b"b")
                    .unwrap()
            ),
            b"b-v1"
        );
        assert_eq!(keydir.get(b"c"), None); // torn write, correctly dropped
    }

    #[test]
    fn corrupt_entry_mid_file_is_skipped_but_scan_continues() {
        let dir = TempDir::new();
        let mut active = ActiveFile::create(&dir, 0).unwrap();
        write_entry(&dir, &mut active, b"a", b"a-v1", false, 1);
        let (b_pos, _, _) = write_entry(&dir, &mut active, b"b", b"b-v1", false, 2);
        write_entry(&dir, &mut active, b"c", b"c-v1", false, 3);
        active
            .sync()
            .unwrap();
        drop(active);

        // Flip a bit inside "b"'s value bytes — CRC will no longer match.
        let path = DataFileSet::data_path(&dir, 0);
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        f.seek(SeekFrom::Start(b_pos))
            .unwrap();
        let mut byte = [0u8; 1];
        f.read_exact(&mut byte)
            .unwrap();
        byte[0] ^= 0xFF;
        f.seek(SeekFrom::Start(b_pos))
            .unwrap();
        f.write_all(&byte)
            .unwrap();
        drop(f);

        let file_ids = DataFileSet::discover(&dir).unwrap();
        let mut keydir = Keydir::new();
        recover(&dir, &file_ids, &mut keydir).unwrap();

        assert_eq!(
            read_value(
                &dir,
                keydir
                    .get(b"a")
                    .unwrap()
            ),
            b"a-v1"
        );
        assert_eq!(keydir.get(b"b"), None); // corrupt entry skipped, not applied
        assert_eq!(
            read_value(
                &dir,
                keydir
                    .get(b"c")
                    .unwrap()
            ),
            b"c-v1"
        );
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
        active0
            .sync()
            .unwrap();
        drop(active0);

        let path0 = DataFileSet::data_path(&dir, 0);
        let full_len = fs::metadata(&path0)
            .unwrap()
            .len();
        OpenOptions::new()
            .write(true)
            .open(&path0)
            .unwrap()
            .set_len(full_len - last_total_len.min(3))
            .unwrap();

        let mut active1 = ActiveFile::create(&dir, 1).unwrap();
        write_entry(&dir, &mut active1, b"c", b"c-v1", false, 3);
        active1
            .sync()
            .unwrap();
        drop(active1);

        let file_ids = DataFileSet::discover(&dir).unwrap();
        let mut keydir = Keydir::new();
        recover(&dir, &file_ids, &mut keydir).unwrap();

        assert_eq!(
            read_value(
                &dir,
                keydir
                    .get(b"a")
                    .unwrap()
            ),
            b"a-v1"
        );
        assert_eq!(keydir.get(b"b"), None);
        assert_eq!(
            read_value(
                &dir,
                keydir
                    .get(b"c")
                    .unwrap()
            ),
            b"c-v1"
        );
    }
}
