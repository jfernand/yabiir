use super::*;
use crate::datafile::ActiveFile;
use crate::format;
use crate::keydir::KeydirEntry;
use crate::testutil::TempDir;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};

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
    let value_pos = active
        .append(&encoded)
        .unwrap();
    (value_pos, encoded.len() as u64, header)
}

fn read_value(dir: &Path, entry: KeydirEntry) -> Vec<u8> {
    DataFileSet::new(dir)
        .read_at(entry.file_id, entry.value_pos, entry.value_size)
        .unwrap()
}

/// Write a well-formed hint file (records + a correct trailing CRC) for
/// `file_id`, so a test can drive the hint-based fast path directly
/// instead of the full-scan fallback.
fn write_valid_hint_file(dir: &Path, file_id: u32, records: &[(&[u8], u64, format::EntryHeader)]) {
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
