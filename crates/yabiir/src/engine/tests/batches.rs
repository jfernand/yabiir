//! Write batches: conditions, atomicity in the session and after a crash,
//! versions, and many writers at once.

use std::fs;
use std::sync::Arc;

use super::*;
use crate::datafile::DataFileSet;
use crate::{Cond, Op};

fn put(key: &[u8], value: &[u8]) -> Op {
    Op::Put {
        key: key.to_vec(),
        value: value.to_vec(),
    }
}

fn delete(key: &[u8]) -> Op {
    Op::Delete { key: key.to_vec() }
}

fn check(key: &[u8], cond: Cond) -> Op {
    Op::Check {
        key: key.to_vec(),
        cond,
    }
}

fn value(db: &Engine, key: &[u8]) -> Option<Vec<u8>> {
    db.get(key)
        .unwrap()
}

fn version(db: &Engine, key: &[u8]) -> Option<u64> {
    db.get_versioned(key)
        .unwrap()
        .map(|v| v.version)
}

#[test]
fn a_batch_writes_every_op_with_one_version() {
    let dir = TempDir::new();
    let db = open(&dir);
    db.put(b"gone", b"x", now_unix())
        .unwrap();
    let written = db
        .write_batch(vec![put(b"a", b"1"), put(b"b", b"2"), delete(b"gone")])
        .wait()
        .unwrap()
        .expect("the batch wrote");
    assert_eq!(value(&db, b"a"), Some(b"1".to_vec()));
    assert_eq!(value(&db, b"b"), Some(b"2".to_vec()));
    assert_eq!(value(&db, b"gone"), None);
    assert_eq!(version(&db, b"a"), Some(written));
    assert_eq!(version(&db, b"b"), Some(written));
}

#[test]
fn later_ops_on_a_key_win_within_a_batch() {
    let dir = TempDir::new();
    let db = open(&dir);
    db.write_batch(vec![put(b"k", b"1"), delete(b"k"), put(b"k", b"3")])
        .wait()
        .unwrap();
    assert_eq!(value(&db, b"k"), Some(b"3".to_vec()));
    db.write_batch(vec![put(b"j", b"1"), delete(b"j")])
        .wait()
        .unwrap();
    assert_eq!(value(&db, b"j"), None);
    drop(db);
    let db = open(&dir);
    assert_eq!(value(&db, b"k"), Some(b"3".to_vec()));
    assert_eq!(value(&db, b"j"), None);
}

#[test]
fn a_failed_check_writes_nothing_and_names_its_position() {
    let dir = TempDir::new();
    let db = open(&dir);
    db.put(b"k", b"old", now_unix())
        .unwrap();
    let v = version(&db, b"k").unwrap();

    let result = db
        .write_batch(vec![
            put(b"other", b"x"),
            check(b"k", Cond::Exists),
            check(b"k", Cond::Version(v + 1)),
            put(b"k", b"new"),
        ])
        .wait();
    assert!(
        matches!(result, Err(Error::ConditionFailed { index: 2 })),
        "{result:?}"
    );
    assert_eq!(value(&db, b"k"), Some(b"old".to_vec()));
    assert_eq!(value(&db, b"other"), None);
    drop(db);
    assert_eq!(
        value(&open(&dir), b"other"),
        None,
        "nothing in the log either"
    );
}

#[test]
fn checks_see_the_store_before_the_batch() {
    let dir = TempDir::new();
    let db = open(&dir);
    // The put comes first in the batch, but the check is against the store
    // as it was: the key is absent.
    db.write_batch(vec![put(b"k", b"1"), check(b"k", Cond::Absent)])
        .wait()
        .unwrap();
    assert_eq!(value(&db, b"k"), Some(b"1".to_vec()));
}

#[test]
fn a_version_check_is_a_compare_and_swap() {
    let dir = TempDir::new();
    let db = open(&dir);
    let created = db
        .write_batch(vec![check(b"k", Cond::Absent), put(b"k", b"1")])
        .wait()
        .unwrap()
        .unwrap();
    // A second create fails: the key exists now.
    assert!(matches!(
        db.write_batch(vec![check(b"k", Cond::Absent), put(b"k", b"x")])
            .wait(),
        Err(Error::ConditionFailed { index: 0 })
    ));
    let updated = db
        .write_batch(vec![check(b"k", Cond::Version(created)), put(b"k", b"2")])
        .wait()
        .unwrap()
        .unwrap();
    assert!(updated > created);
    // The same swap again is stale.
    assert!(matches!(
        db.write_batch(vec![check(b"k", Cond::Version(created)), put(b"k", b"3")])
            .wait(),
        Err(Error::ConditionFailed { index: 0 })
    ));
    // Delete and recreate: the old version doesn't match the new value.
    db.delete(b"k", now_unix())
        .unwrap();
    db.put(b"k", b"again", now_unix())
        .unwrap();
    assert!(matches!(
        db.write_batch(vec![check(b"k", Cond::Version(updated)), put(b"k", b"4")])
            .wait(),
        Err(Error::ConditionFailed { index: 0 })
    ));
    assert_eq!(value(&db, b"k"), Some(b"again".to_vec()));
}

#[test]
fn a_batch_with_nothing_to_write_resolves_to_none() {
    let dir = TempDir::new();
    let db = open(&dir);
    assert_eq!(
        db.write_batch(vec![check(b"k", Cond::Absent), delete(b"k")])
            .wait()
            .unwrap(),
        None
    );
    assert_eq!(
        db.write_batch(Vec::new())
            .wait()
            .unwrap(),
        None
    );
    assert!(
        DataFileSet::discover(&dir)
            .unwrap()
            .is_empty(),
        "nothing to write creates no file"
    );
}

#[test]
fn invalid_batches_fail_before_reaching_the_writer() {
    let dir = TempDir::new();
    let db = open(&dir);
    assert!(matches!(
        db.write_batch(vec![put(b"ok", b"1"), put(b"", b"2")])
            .wait(),
        Err(Error::EmptyKey)
    ));
    assert!(matches!(
        db.write_batch(vec![check(b"", Cond::Absent)])
            .wait(),
        Err(Error::EmptyKey)
    ));
    assert_eq!(value(&db, b"ok"), None);

    let read_only = Engine::open(&*dir, Options::default().read_only()).unwrap();
    assert!(matches!(
        read_only
            .write_batch(vec![put(b"k", b"v")])
            .wait(),
        Err(Error::ReadOnly)
    ));
}

/// Commits are futures too; this drives one with a minimal executor.
#[test]
fn a_commit_can_be_awaited() {
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    let dir = TempDir::new();
    let db = open(&dir);
    let mut commit = pin!(db.write_batch(vec![put(b"k", b"v")]));
    let mut cx = Context::from_waker(Waker::noop());
    let outcome = loop {
        if let Poll::Ready(outcome) = commit
            .as_mut()
            .poll(&mut cx)
        {
            break outcome;
        }
        std::thread::yield_now();
    };
    assert!(
        outcome
            .unwrap()
            .is_some()
    );
    assert_eq!(value(&db, b"k"), Some(b"v".to_vec()));
}

/// Many threads increment one counter with compare-and-swap: every
/// increment that commits must count, and none twice.
#[test]
fn concurrent_compare_and_swaps_lose_no_increment() {
    let dir = TempDir::new();
    let db = Arc::new(open(&dir));
    db.put(b"n", b"0", now_unix())
        .unwrap();
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let db = Arc::clone(&db);
            std::thread::spawn(move || {
                let mut committed = 0;
                while committed < 25 {
                    let current = db
                        .get_versioned(b"n")
                        .unwrap()
                        .unwrap();
                    let n: u64 = String::from_utf8(current.value)
                        .unwrap()
                        .parse()
                        .unwrap();
                    let next = (n + 1)
                        .to_string()
                        .into_bytes();
                    match db
                        .write_batch(vec![
                            check(b"n", Cond::Version(current.version)),
                            put(b"n", &next),
                        ])
                        .wait()
                    {
                        Ok(_) => committed += 1,
                        Err(Error::ConditionFailed { .. }) => {}
                        Err(e) => panic!("{e}"),
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join()
            .unwrap();
    }
    assert_eq!(value(&db, b"n"), Some(b"200".to_vec()));
}

/// A crash can stop an append anywhere. Cut the file holding a batch at
/// every byte of the batch's record: after a reopen the store holds either
/// all of the batch or none of it.
#[test]
fn a_batch_cut_anywhere_by_a_crash_recovers_whole_or_not_at_all() {
    let dir = TempDir::new();
    let db = open(&dir);
    db.put(b"a", b"before", now_unix())
        .unwrap();
    let path = DataFileSet::data_path(
        &dir,
        *DataFileSet::discover(&dir)
            .unwrap()
            .last()
            .unwrap(),
    );
    let batch_start = fs::metadata(&path)
        .unwrap()
        .len();
    db.write_batch(vec![
        put(b"a", b"after"),
        put(b"b", b"new"),
        delete(b"a"),
        put(b"c", &[7u8; 300]),
    ])
    .wait()
    .unwrap();
    drop(db);
    let whole = fs::read(&path).unwrap();
    assert!(whole.len() as u64 > batch_start);

    for cut in batch_start..whole.len() as u64 {
        let crashed = TempDir::new();
        fs::write(DataFileSet::data_path(&crashed, 0), &whole[..cut as usize]).unwrap();
        let db = open(&crashed);
        assert_eq!(value(&db, b"a"), Some(b"before".to_vec()), "cut at {cut}");
        assert_eq!(value(&db, b"b"), None, "cut at {cut}");
        assert_eq!(value(&db, b"c"), None, "cut at {cut}");
    }
    let db = open(&dir);
    assert_eq!(value(&db, b"a"), None);
    assert_eq!(value(&db, b"b"), Some(b"new".to_vec()));
    assert_eq!(value(&db, b"c"), Some(vec![7u8; 300]));
}

#[test]
fn a_batch_survives_merge_and_reopen() {
    let dir = TempDir::new();
    let db = Engine::open(
        &*dir,
        Options {
            max_file_size: 1,
            ..Options::default()
        },
    )
    .unwrap();
    let v = db
        .write_batch(vec![put(b"a", b"1"), put(b"b", b"2"), put(b"c", b"3")])
        .wait()
        .unwrap()
        .unwrap();
    db.put(b"b", b"22", now_unix())
        .unwrap();
    db.merge()
        .unwrap();
    assert_eq!(version(&db, b"a"), Some(v));
    drop(db);
    let db = open(&dir);
    assert_eq!(value(&db, b"a"), Some(b"1".to_vec()));
    assert_eq!(value(&db, b"b"), Some(b"22".to_vec()));
    assert_eq!(version(&db, b"c"), Some(v));
}
