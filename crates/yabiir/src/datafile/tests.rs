use super::name::parse_data_file_id;
use super::*;
use crate::format;
use crate::testutil::TempDir;
use std::fs;
use std::sync::Arc;
use std::thread;

#[test]
fn append_then_read_back_matches() {
    let dir = TempDir::new();
    let mut active = ActiveFile::create(&dir, 1).unwrap();

    let cases: [(&[u8], &[u8]); 3] = [(b"a", b"1"), (b"bb", b"22"), (b"ccc", b"")];
    let mut locations = Vec::new();
    for (key, value) in &cases {
        let encoded = format::encode_entry(key, value, false, 0);
        let value_pos = active
            .append(&encoded)
            .unwrap();
        let file_id = active.file_id();
        locations.push((file_id, value_pos, value.len() as u32));
    }
    active
        .sync()
        .unwrap();
    drop(active); // now immutable

    let files = DataFileSet::new(&*dir);
    for ((file_id, value_pos, value_len), (_, expected_value)) in locations
        .iter()
        .zip(cases.iter())
    {
        let read = files
            .read_at(*file_id, *value_pos, *value_len)
            .unwrap();
        assert_eq!(&read, expected_value);
    }
}

#[test]
fn manual_rotation_produces_files_with_increasing_ids_and_expected_sizes() {
    let dir = TempDir::new();
    let threshold: u64 = 200;
    let mut file_id = 1u32;
    let mut active = ActiveFile::create(&dir, file_id).unwrap();
    let mut rotations = 0;

    for i in 0..50u32 {
        let key = format!("key-{i}");
        let value = vec![b'x'; 20];
        let encoded = format::encode_entry(key.as_bytes(), &value, false, 0);
        active
            .append(&encoded)
            .unwrap();
        if active.len() >= threshold {
            active
                .sync()
                .unwrap();
            file_id += 1;
            active = ActiveFile::create(&dir, file_id).unwrap();
            rotations += 1;
        }
    }
    active
        .sync()
        .unwrap();
    drop(active);

    assert!(
        rotations >= 3,
        "expected several rotations, got {rotations}"
    );

    let ids = DataFileSet::discover(&dir).unwrap();
    let expected: Vec<u32> = (1..=file_id).collect();
    assert_eq!(ids, expected);

    // every file except the last (still being written when the loop
    // ended) should be at or above the rotation threshold.
    for &id in &ids[..ids.len() - 1] {
        let meta = fs::metadata(DataFileSet::data_path(&dir, id)).unwrap();
        assert!(
            meta.len() >= threshold,
            "file {id} is {} bytes, expected >= {threshold}",
            meta.len()
        );
    }
}

#[test]
fn concurrent_reads_on_same_file_id_are_correct() {
    let dir = TempDir::new();
    let mut active = ActiveFile::create(&dir, 1).unwrap();

    let mut locations = Vec::new();
    for i in 0..64u32 {
        let value = vec![(i % 256) as u8; 37]; // distinct-ish content per index
        let encoded = format::encode_entry(format!("k{i}").as_bytes(), &value, false, 0);
        let value_pos = active
            .append(&encoded)
            .unwrap();
        let file_id = active.file_id();
        locations.push((file_id, value_pos, value));
    }
    active
        .sync()
        .unwrap();
    drop(active);

    let files = Arc::new(DataFileSet::new(&*dir));
    let locations = Arc::new(locations);

    let handles: Vec<_> = (0..8u32)
        .map(|t| {
            let files = Arc::clone(&files);
            let locations = Arc::clone(&locations);
            thread::spawn(move || {
                for (idx, (file_id, value_pos, expected)) in locations
                    .iter()
                    .enumerate()
                {
                    if idx as u32 % 8 != t {
                        continue; // spread work across threads, all hitting the same file_id
                    }
                    let read = files
                        .read_at(*file_id, *value_pos, expected.len() as u32)
                        .unwrap();
                    assert_eq!(&read, expected, "thread {t} idx {idx}");
                }
            })
        })
        .collect();
    for h in handles {
        h.join()
            .unwrap();
    }
}

#[test]
fn is_empty_reflects_whether_anything_has_been_written() {
    let dir = TempDir::new();
    let mut active = ActiveFile::create(&dir, 1).unwrap();
    assert!(active.is_empty());
    let encoded = format::encode_entry(b"k", b"v", false, 0);
    active
        .append(&encoded)
        .unwrap();
    assert!(!active.is_empty());
}

#[test]
fn active_file_read_at_returns_the_bytes_just_written() {
    let dir = TempDir::new();
    let mut active = ActiveFile::create(&dir, 1).unwrap();
    let encoded = format::encode_entry(b"k", b"hello", false, 0);
    let value_pos = active
        .append(&encoded)
        .unwrap();
    // Read directly through ActiveFile's own handle, not DataFileSet —
    // this is the still-active file, and DataFileSet only ever opens
    // rotated-out (immutable) files.
    assert_eq!(
        active
            .read_at(value_pos, 5)
            .unwrap(),
        b"hello"
    );
}

#[test]
fn flush_makes_buffered_bytes_visible_on_disk() {
    let dir = TempDir::new();
    let mut active = ActiveFile::create(&dir, 1).unwrap();
    let encoded = format::encode_entry(b"k", b"v", false, 0);
    active
        .append_buffered(&encoded)
        .unwrap(); // deliberately not flushed
    let path = DataFileSet::data_path(&dir, 1);
    let before = fs::metadata(&path)
        .unwrap()
        .len();
    assert!(
        before < active.len(),
        "bytes should still be buffered, not yet on disk"
    );

    active
        .flush()
        .unwrap();

    let after = fs::metadata(&path)
        .unwrap()
        .len();
    assert_eq!(
        after,
        active.len(),
        "flush should make buffered bytes visible on disk"
    );
}

/// The rule `DataFileSet::handle` relies on: a file created through
/// `create_active`, or registered at open, has its handle cached before
/// anything points into it, so it stays readable through `handle` after
/// the file is unlinked — no `open` by path is ever needed.
#[test]
fn create_active_and_register_cache_handles_that_outlive_unlink() {
    let dir = TempDir::new();
    let encoded = format::encode_entry(b"k", b"v", false, 0);

    // A file that already exists when the set is created (recovery's case).
    let mut existing = ActiveFile::create(&dir, 1).unwrap();
    let existing_pos = existing
        .append(&encoded)
        .unwrap();
    drop(existing);

    let files = DataFileSet::new(&*dir);
    files
        .register(&[1])
        .unwrap();

    // A file created through the set (rotation's and merge output's case).
    let mut created = files
        .create_active(2)
        .unwrap();
    let created_pos = created
        .append(&encoded)
        .unwrap();

    for (file_id, pos) in [(1, existing_pos), (2, created_pos)] {
        fs::remove_file(DataFileSet::data_path(&dir, file_id)).unwrap();
        let file = files
            .handle(file_id)
            .unwrap();
        assert_eq!(pread_exact(&file, pos, 1).unwrap(), b"v");
    }
}

#[test]
fn forget_evicts_the_cached_read_handle() {
    let dir = TempDir::new();
    let mut active = ActiveFile::create(&dir, 1).unwrap();
    let encoded = format::encode_entry(b"k", b"v", false, 0);
    let value_pos = active
        .append(&encoded)
        .unwrap();
    let file_id = active.file_id();
    active
        .sync()
        .unwrap();
    drop(active);

    let files = DataFileSet::new(&*dir);
    // Populate the handle cache.
    assert_eq!(
        files
            .read_at(file_id, value_pos, 1)
            .unwrap(),
        b"v"
    );

    fs::remove_file(DataFileSet::data_path(&dir, file_id)).unwrap();
    // Unix semantics: the cached fd stays valid even after the
    // directory entry is unlinked, so without forgetting it, reads
    // still (silently) succeed against the deleted file's old content.
    assert_eq!(
        files
            .read_at(file_id, value_pos, 1)
            .unwrap(),
        b"v"
    );

    files.forget(file_id);
    // Forgetting evicts the cached fd, so the next read has to do a
    // fresh File::open — which now fails, since the file is gone.
    assert!(
        files
            .read_at(file_id, value_pos, 1)
            .is_err()
    );
}

#[test]
fn parse_data_file_id_requires_both_correct_length_and_all_digits() {
    assert_eq!(
        parse_data_file_id(&format!("{:020}.bitcask.data", 42)),
        Some(42)
    );
    // Right suffix, all-digit stem, but wrong length — must still be
    // rejected (a real Bitcask file is always exactly 20 digits).
    assert_eq!(parse_data_file_id("42.bitcask.data"), None);
    // Exactly 20 characters, but not all digits.
    assert_eq!(
        parse_data_file_id(&format!("{:019}x.bitcask.data", 42)),
        None
    );
    assert_eq!(parse_data_file_id("not-a-data-file.txt"), None);
}

#[test]
fn a_scan_stops_at_a_header_claiming_more_than_the_file_holds() {
    let dir = TempDir::new();
    let good = format::encode_entry(b"key", b"value", false, 0).into_bytes();
    let mut torn = format::encode_entry(b"next", b"value", false, 0).into_bytes();
    torn[12..16].copy_from_slice(&u32::MAX.to_le_bytes()); // value_size
    let path = DataFileSet::data_path(&dir, 1);
    fs::write(&path, [good.as_slice(), torn.as_slice()].concat()).unwrap();

    let scanned: Vec<Scanned> = scan(&path)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(scanned.len(), 2, "got {scanned:?}");
    assert!(
        matches!(&scanned[0], Scanned::Entry { offset: 0, entry } if entry.key == b"key"),
        "got {:?}",
        scanned[0]
    );
    assert!(
        matches!(scanned[1], Scanned::Truncated { offset } if offset == good.len() as u64),
        "got {:?}",
        scanned[1]
    );
}
