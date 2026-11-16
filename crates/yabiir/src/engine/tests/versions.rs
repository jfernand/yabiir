//! Versions: every write's sequence number, and how stores written before
//! there were versions read.

use std::fs;

use super::*;
use crate::datafile::DataFileSet;
use crate::format::{self, DATA_MAGIC, EntryHeader, HINT_MAGIC};
use crate::{KeyOrder, Versioned};

fn version_of(db: &Engine, key: &[u8]) -> Option<u64> {
    db.get_versioned(key)
        .unwrap()
        .map(|found| found.version)
}

#[test]
fn every_put_gives_its_key_a_higher_version() {
    let dir = TempDir::new();
    let db = open(&dir);
    db.put(b"a", b"1", now_unix())
        .unwrap();
    let first = version_of(&db, b"a").unwrap();
    db.put(b"b", b"1", now_unix())
        .unwrap();
    db.put(b"a", b"2", now_unix())
        .unwrap();
    let second = version_of(&db, b"a").unwrap();
    assert!(second > first, "{first} then {second}");
    assert_eq!(
        db.get_versioned(b"a")
            .unwrap(),
        Some(Versioned {
            value: b"2".to_vec(),
            version: second,
        })
    );
    assert_eq!(version_of(&db, b"missing"), None);
}

#[test]
fn versions_survive_a_reopen_from_the_data_files_and_from_hints() {
    let dir = TempDir::new();
    let db = open(&dir);
    for (k, v) in [(b"a", b"1"), (b"b", b"2"), (b"a", b"3")] {
        db.put(k, v, now_unix())
            .unwrap();
    }
    let before = [version_of(&db, b"a"), version_of(&db, b"b")];
    drop(db);

    let db = open(&dir);
    assert_eq!([version_of(&db, b"a"), version_of(&db, b"b")], before);
    db.merge()
        .unwrap();
    assert_eq!([version_of(&db, b"a"), version_of(&db, b"b")], before);
    drop(db);

    // Now from the hint files merge wrote.
    let db = open(&dir);
    assert_eq!([version_of(&db, b"a"), version_of(&db, b"b")], before);
}

/// The version a deleted key had must not be handed out again, even once
/// merge has dropped the tombstone that was the only record of the delete
/// and the store has been reopened: the new value would carry a version a
/// client may still hold for the old one.
#[test]
fn a_version_is_never_reused_after_a_delete_a_merge_and_a_reopen() {
    let dir = TempDir::new();
    let db = open(&dir);
    db.put(b"keep", b"x", now_unix())
        .unwrap();
    db.put(b"k", b"old", now_unix())
        .unwrap();
    let old = version_of(&db, b"k").unwrap();
    db.delete(b"k", now_unix())
        .unwrap();
    db.merge()
        .unwrap();
    drop(db);

    let db = open(&dir);
    db.put(b"k", b"new", now_unix())
        .unwrap();
    let new = version_of(&db, b"k").unwrap();
    // The delete took the version after `old`, so the new put is two past.
    assert!(new > old + 1, "old {old}, new {new}");
}

/// The same, when the merge kept nothing at all: no live key, so no record
/// to copy. The merge's output must still carry the sequence floor, or a
/// store left with no files reopens counting from 1.
#[test]
fn a_version_is_never_reused_after_a_merge_that_kept_nothing() {
    let dir = TempDir::new();
    let db = open(&dir);
    db.put(b"k", b"old", now_unix())
        .unwrap();
    let old = version_of(&db, b"k").unwrap();
    db.delete(b"k", now_unix())
        .unwrap();
    db.merge()
        .unwrap();
    db.close()
        .unwrap();

    let db = open(&dir);
    db.put(b"k", b"new", now_unix())
        .unwrap();
    let new = version_of(&db, b"k").unwrap();
    assert!(new > old + 1, "old {old}, new {new}");
}

/// Data and hint files as yabiir 0.8 wrote them: no header, entries
/// instead of records.
fn write_v1_store(dir: &std::path::Path) {
    let mut file0 = Vec::new();
    for (k, v, tombstone) in [
        (&b"a"[..], &b"a-old"[..], false),
        (b"b", b"b-1", false),
        (b"gone", b"x", false),
        (b"gone", b"", true),
    ] {
        file0.extend_from_slice(format::encode_entry(k, v, tombstone, 1_700_000_000).as_bytes());
    }
    fs::write(DataFileSet::data_path(dir, 0), file0).unwrap();

    // File 1 as a 0.8 merge left it: a data file with a v1 hint beside it.
    let entry = format::encode_entry(b"a", b"a-new", false, 1_700_000_001);
    fs::write(DataFileSet::data_path(dir, 1), entry.as_bytes()).unwrap();
    let header = EntryHeader {
        timestamp: 1_700_000_001,
        key_size: 1,
        value_size: 5,
        tombstone: false,
    };
    let value_pos = (format::HEADER_SIZE + 1) as u64;
    let mut hint = format::encode_hint(b"a", &header, value_pos).into_bytes();
    let crc = crc32fast::hash(&hint);
    hint.extend_from_slice(&crc.to_le_bytes());
    fs::write(DataFileSet::hint_path(dir, 1), hint).unwrap();
}

fn assert_v1_contents(db: &Engine) {
    assert_eq!(
        db.get(b"a")
            .unwrap(),
        Some(b"a-new".to_vec())
    );
    assert_eq!(
        db.get(b"b")
            .unwrap(),
        Some(b"b-1".to_vec())
    );
    assert_eq!(
        db.get(b"gone")
            .unwrap(),
        None
    );
}

#[test]
fn a_store_written_in_format_1_opens_with_every_version_at_one() {
    for order in [KeyOrder::Hashed, KeyOrder::Sorted] {
        let dir = TempDir::new();
        write_v1_store(&dir);
        let db = Engine::open(
            &*dir,
            Options {
                key_order: order,
                ..Options::default()
            },
        )
        .unwrap();
        assert_v1_contents(&db);
        assert_eq!(version_of(&db, b"a"), Some(1), "{order:?}");
        assert_eq!(version_of(&db, b"b"), Some(1), "{order:?}");

        db.put(b"b", b"b-2", now_unix())
            .unwrap();
        assert!(version_of(&db, b"b").unwrap() > 1, "{order:?}");
    }
}

#[test]
fn merge_rewrites_a_format_1_store_in_format_2() {
    let dir = TempDir::new();
    write_v1_store(&dir);
    let db = open(&dir);
    db.merge()
        .unwrap();
    assert_v1_contents(&db);
    drop(db);

    for id in DataFileSet::discover(&dir).unwrap() {
        let data = fs::read(DataFileSet::data_path(&dir, id)).unwrap();
        assert_eq!(data[..8], DATA_MAGIC, "data file {id}");
        let hint = fs::read(DataFileSet::hint_path(&dir, id)).unwrap();
        assert_eq!(hint[..8], HINT_MAGIC, "hint file {id}");
    }
    let db = open(&dir);
    assert_v1_contents(&db);
    assert_eq!(version_of(&db, b"a"), Some(1));
}
