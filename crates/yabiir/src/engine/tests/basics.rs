use super::*;
use crate::datafile::DataFileSet;

#[test]
fn put_then_get_returns_value() {
    let dir = TempDir::new();
    let db = open(&dir);
    db.put(b"k", b"v", now_unix())
        .unwrap();
    assert_eq!(
        db.get(b"k")
            .unwrap(),
        Some(b"v".to_vec())
    );
}

#[test]
fn put_twice_returns_second_value_only() {
    let dir = TempDir::new();
    let db = open(&dir);
    db.put(b"k", b"v1", now_unix())
        .unwrap();
    db.put(b"k", b"v2", now_unix())
        .unwrap();
    assert_eq!(
        db.get(b"k")
            .unwrap(),
        Some(b"v2".to_vec())
    );
}

#[test]
fn delete_then_get_returns_none() {
    let dir = TempDir::new();
    let db = open(&dir);
    db.put(b"k", b"v", now_unix())
        .unwrap();
    db.delete(b"k", now_unix())
        .unwrap();
    assert_eq!(
        db.get(b"k")
            .unwrap(),
        None
    );
}

#[test]
fn delete_missing_key_is_a_silent_no_op() {
    let dir = TempDir::new();
    let db = open(&dir);
    db.delete(b"missing", now_unix())
        .unwrap(); // must not error
    assert_eq!(
        db.get(b"missing")
            .unwrap(),
        None
    );
}

#[test]
fn get_missing_key_returns_none() {
    let dir = TempDir::new();
    let db = open(&dir);
    assert_eq!(
        db.get(b"missing")
            .unwrap(),
        None
    );
}

#[test]
fn various_value_sizes_round_trip() {
    let dir = TempDir::new();
    let db = open(&dir);
    for (key, len) in [
        (b"empty".as_slice(), 0),
        (b"one", 1),
        (b"big", 3 * 1024 * 1024),
    ] {
        let value = vec![0x5Au8; len];
        db.put(key, &value, now_unix())
            .unwrap();
        assert_eq!(
            db.get(key)
                .unwrap(),
            Some(value)
        );
    }
}

#[test]
fn many_distinct_keys_round_trip() {
    let dir = TempDir::new();
    let db = open(&dir);
    for i in 0..10_000u32 {
        db.put(
            format!("key-{i}").as_bytes(),
            format!("value-{i}").as_bytes(),
            now_unix(),
        )
        .unwrap();
    }
    for i in 0..10_000u32 {
        assert_eq!(
            db.get(format!("key-{i}").as_bytes())
                .unwrap(),
            Some(format!("value-{i}").into_bytes())
        );
    }
}

#[test]
fn put_rejects_empty_key() {
    let dir = TempDir::new();
    let db = open(&dir);
    assert!(matches!(
        db.put(b"", b"v", now_unix()),
        Err(Error::EmptyKey)
    ));
}

#[test]
fn delete_rejects_empty_key() {
    let dir = TempDir::new();
    let db = open(&dir);
    assert!(matches!(db.delete(b"", now_unix()), Err(Error::EmptyKey)));
}

#[test]
fn put_and_delete_are_rejected_on_a_read_only_handle() {
    let dir = TempDir::new();
    {
        let db = open(&dir); // create the directory as a writer first
        db.put(b"k", b"v", now_unix())
            .unwrap();
    }
    let ro = Engine::open(
        &*dir,
        Options {
            is_read_write: false,
            ..Options::default()
        },
    )
    .unwrap();
    assert!(matches!(
        ro.put(b"k2", b"v2", now_unix()),
        Err(Error::ReadOnly)
    ));
    assert!(matches!(ro.delete(b"k", now_unix()), Err(Error::ReadOnly)));
}

#[test]
fn list_keys_and_fold_reflect_live_keys_only() {
    let dir = TempDir::new();
    let db = open(&dir);
    db.put(b"a", b"1", now_unix())
        .unwrap();
    db.put(b"b", b"2", now_unix())
        .unwrap();
    db.put(b"c", b"3", now_unix())
        .unwrap();
    db.delete(b"b", now_unix())
        .unwrap();

    let mut keys = db
        .list_keys()
        .unwrap();
    keys.sort();
    assert_eq!(keys, vec![b"a".to_vec(), b"c".to_vec()]);

    let total: usize = db
        .fold(|_k, v, acc| acc + v.len(), 0usize)
        .unwrap();
    assert_eq!(total, 2); // "1".len() + "3".len()
}

#[test]
fn rotation_keeps_every_key_readable_across_both_active_and_rotated_files() {
    let dir = TempDir::new();
    let db = Engine::open(
        &*dir,
        Options {
            max_file_size: 300,
            ..Options::default()
        },
    )
    .unwrap();

    for i in 0..200u32 {
        db.put(
            format!("k{i}").as_bytes(),
            format!("value-{i}").as_bytes(),
            now_unix(),
        )
        .unwrap();
    }

    // Must have actually rotated for this test to be exercising what it
    // claims to.
    let file_count = DataFileSet::discover(&dir)
        .unwrap()
        .len();
    assert!(
        file_count > 1,
        "expected multiple data files, got {file_count}"
    );

    for i in 0..200u32 {
        assert_eq!(
            db.get(format!("k{i}").as_bytes())
                .unwrap(),
            Some(format!("value-{i}").into_bytes())
        );
    }
}

/// The entry header has 31 bits for the key's length and 32 for the
/// value's. Anything longer is refused: written, it would corrupt the file.
/// (Checked on the lengths alone; nobody allocates 4 GiB for a test.)
#[test]
fn keys_and_values_too_large_for_the_header_are_refused() {
    use crate::engine::write::{check_key_len, check_value_len};

    assert!(matches!(check_key_len(0), Err(Error::EmptyKey)));
    assert!(check_key_len(1).is_ok());
    assert!(check_key_len((1 << 31) - 1).is_ok());
    assert!(matches!(check_key_len(1 << 31), Err(Error::KeyTooLarge)));

    assert!(check_value_len(0).is_ok());
    assert!(check_value_len(u32::MAX as usize).is_ok());
    assert!(matches!(
        check_value_len(u32::MAX as usize + 1),
        Err(Error::ValueTooLarge)
    ));
}
