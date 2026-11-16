//! Property-based model test (`docs/bitcask-implementation-plan.md` §10):
//! random sequences of put/delete/get/reopen/merge, checked against a plain
//! `BTreeMap` reference model — with a hashed keydir, a sorted one, and one
//! switching order at every reopen; for a sorted keydir, key order too. The highest-leverage test in the whole plan
//! for a storage engine like this — real bugs in a log-structured store
//! tend to show up only after specific *interleavings* of writes, deletes,
//! reopens and merges that a hand-written test case is unlikely to think
//! to try, but a few hundred random ones very likely will.
//!
//! Black-box: only the public `yabiir` API is used, exactly as any real
//! caller would use it.

use std::collections::BTreeMap;
use std::ops::Bound;

use proptest::prelude::*;
use yabiir::{Bitcask, Cond, Direction, Engine, Error, KeyOrder, Options, now_unix};
use yabiir_testutil::TempDir;

#[derive(Debug, Clone)]
enum Op {
    Put(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
    Get(Vec<u8>),
    Reopen,
    Merge,
    Batch(Vec<BatchOp>),
}

/// One operation of a write batch, before versions are known: a version
/// check is against the key's current version, or a stale one.
#[derive(Debug, Clone)]
enum BatchOp {
    Put(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
    CheckAbsent(Vec<u8>),
    CheckExists(Vec<u8>),
    CheckCurrentVersion(Vec<u8>),
    CheckStaleVersion(Vec<u8>),
}

fn batch_op_strategy() -> impl Strategy<Value = BatchOp> {
    prop_oneof![
        4 => (key_strategy(), value_strategy()).prop_map(|(k, v)| BatchOp::Put(k, v)),
        2 => key_strategy().prop_map(BatchOp::Delete),
        1 => key_strategy().prop_map(BatchOp::CheckAbsent),
        1 => key_strategy().prop_map(BatchOp::CheckExists),
        2 => key_strategy().prop_map(BatchOp::CheckCurrentVersion),
        1 => key_strategy().prop_map(BatchOp::CheckStaleVersion),
    ]
}

/// A small, overlapping key alphabet (single bytes 'a'..='f') — deliberately
/// narrow so puts/deletes on the same key collide constantly across an op
/// sequence, which is where interesting last-write-wins/merge-race bugs
/// live, per the plan's own guidance on this test.
fn key_strategy() -> impl Strategy<Value = Vec<u8>> {
    (b'a'..=b'f').prop_map(|b| vec![b])
}

fn value_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..8)
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (key_strategy(), value_strategy()).prop_map(|(k, v)| Op::Put(k, v)),
        4 => key_strategy().prop_map(Op::Get),
        2 => key_strategy().prop_map(Op::Delete),
        1 => Just(Op::Reopen),
        1 => Just(Op::Merge),
        3 => prop::collection::vec(batch_op_strategy(), 0..5).prop_map(Op::Batch),
    ]
}

/// Small `max_file_size` so a typical op sequence actually exercises
/// rotation (and gives merge non-trivial input) instead of everything
/// landing in one never-rotated active file.
fn small_file_options(key_order: KeyOrder) -> Options {
    Options {
        max_file_size: 200,
        key_order,
        ..Options::default()
    }
}

/// How the store is opened across the run: always one order, or switching
/// order at every reopen (nothing about the order is on disk, so a store must
/// open correctly either way).
#[derive(Clone, Copy, Debug)]
enum Orders {
    Always(KeyOrder),
    Alternating,
}

fn run_model(ops: Vec<Op>, orders: Orders) {
    let dir = TempDir::new();
    let mut model: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    // Each live key's version, as the store reported it.
    let mut versions: BTreeMap<Vec<u8>, u64> = BTreeMap::new();
    let mut order = match orders {
        Orders::Always(o) => o,
        Orders::Alternating => KeyOrder::Hashed,
    };
    let mut db = Engine::open(&*dir, small_file_options(order)).unwrap();

    for op in ops {
        match op {
            Op::Put(k, v) => {
                db.put(&k, &v, now_unix())
                    .unwrap();
                let version = db
                    .get_versioned(&k)
                    .unwrap()
                    .unwrap()
                    .version;
                assert!(
                    versions
                        .get(&k)
                        .is_none_or(|&old| version > old)
                );
                versions.insert(k.clone(), version);
                model.insert(k, v);
            }
            Op::Delete(k) => {
                db.delete(&k, now_unix())
                    .unwrap();
                versions.remove(&k);
                model.remove(&k);
            }
            Op::Batch(batch) => {
                run_batch(&db, batch, &mut model, &mut versions);
            }
            Op::Get(k) => {
                let got = db
                    .get_versioned(&k)
                    .unwrap();
                assert_eq!(
                    got.as_ref()
                        .map(|g| &g.value),
                    model.get(&k)
                );
                assert_eq!(
                    got.map(|g| g.version),
                    versions
                        .get(&k)
                        .copied()
                );
            }
            Op::Reopen => {
                drop(db);
                if let Orders::Alternating = orders {
                    order = match order {
                        KeyOrder::Hashed => KeyOrder::Sorted,
                        KeyOrder::Sorted => KeyOrder::Hashed,
                    };
                }
                db = Engine::open(&*dir, small_file_options(order)).unwrap();
            }
            Op::Merge => {
                db.merge()
                    .unwrap();
            }
        }
    }

    // Final full comparison, not just the interleaved Get checks above —
    // catches anything a Get happened not to probe during the sequence.
    let mut keys: Vec<_> = db
        .list_keys()
        .unwrap();
    let model_keys: Vec<_> = model
        .keys()
        .cloned()
        .collect();
    if order == KeyOrder::Sorted {
        // A sorted keydir lists keys in order, and scans them with values.
        assert_eq!(keys, model_keys);
        let all: Vec<(Vec<u8>, Vec<u8>)> = model
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        assert_eq!(
            db.scan(Bound::Unbounded, Bound::Unbounded)
                .unwrap(),
            all
        );
        // Paged, both ways, with versions.
        for limit in 1..=3 {
            for direction in [Direction::Forward, Direction::Reverse] {
                let mut want: Vec<(Vec<u8>, Vec<u8>, u64)> = model
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone(), versions[k]))
                    .collect();
                if direction == Direction::Reverse {
                    want.reverse();
                }
                assert_eq!(scan_all_pages(&db, direction, limit), want);
            }
        }
    }
    keys.sort();
    assert_eq!(keys, model_keys);
    for k in &keys {
        assert_eq!(
            db.get(k)
                .unwrap()
                .as_ref(),
            model.get(k)
        );
    }
}

/// Every live key, value and version, read `limit` at a time.
fn scan_all_pages(db: &Engine, direction: Direction, limit: usize) -> Vec<(Vec<u8>, Vec<u8>, u64)> {
    let mut all = Vec::new();
    let mut after: Option<Vec<u8>> = None;
    loop {
        let bounds = match (&after, direction) {
            (None, _) => (Bound::Unbounded, Bound::Unbounded),
            (Some(a), Direction::Forward) => (Bound::Excluded(a.as_slice()), Bound::Unbounded),
            (Some(a), Direction::Reverse) => (Bound::Unbounded, Bound::Excluded(a.as_slice())),
        };
        let page = db
            .scan_page(bounds.0, bounds.1, direction, limit)
            .unwrap();
        let is_last = page.len() < limit;
        after = page
            .last()
            .map(|(k, _)| k.clone());
        all.extend(
            page.into_iter()
                .map(|(k, found)| (k, found.value, found.version)),
        );
        if is_last {
            return all;
        }
    }
}

/// Run one batch against the store and the model: the checks against the
/// state before the batch, then the writes in order, all or nothing.
fn run_batch(
    db: &Engine,
    batch: Vec<BatchOp>,
    model: &mut BTreeMap<Vec<u8>, Vec<u8>>,
    versions: &mut BTreeMap<Vec<u8>, u64>,
) {
    let mut ops = Vec::new();
    let mut first_failure = None;
    for (index, op) in batch
        .into_iter()
        .enumerate()
    {
        let current = |k: &Vec<u8>| {
            versions
                .get(k)
                .copied()
        };
        let (op, holds) = match op {
            BatchOp::Put(key, value) => (yabiir::Op::Put { key, value }, true),
            BatchOp::Delete(key) => (yabiir::Op::Delete { key }, true),
            BatchOp::CheckAbsent(key) => {
                let holds = current(&key).is_none();
                (
                    yabiir::Op::Check {
                        key,
                        cond: Cond::Absent,
                    },
                    holds,
                )
            }
            BatchOp::CheckExists(key) => {
                let holds = current(&key).is_some();
                (
                    yabiir::Op::Check {
                        key,
                        cond: Cond::Exists,
                    },
                    holds,
                )
            }
            BatchOp::CheckCurrentVersion(key) => {
                // Absent keys have no version: check one that can't match.
                let (cond, holds) = match current(&key) {
                    Some(v) => (Cond::Version(v), true),
                    None => (Cond::Version(1), false),
                };
                (yabiir::Op::Check { key, cond }, holds)
            }
            BatchOp::CheckStaleVersion(key) => {
                let stale = current(&key).map_or(1, |v| v - 1);
                (
                    yabiir::Op::Check {
                        key,
                        cond: Cond::Version(stale),
                    },
                    false,
                )
            }
        };
        if !holds && first_failure.is_none() {
            first_failure = Some(index);
        }
        ops.push(op);
    }

    let outcome = db
        .write_batch(ops.clone())
        .wait();
    if let Some(index) = first_failure {
        assert!(
            matches!(outcome, Err(Error::ConditionFailed { index: i }) if i == index),
            "expected check {index} to fail, got {outcome:?}"
        );
        return;
    }
    let written = outcome.unwrap();
    let mut wrote = false;
    for op in ops {
        match op {
            yabiir::Op::Put { key, value } => {
                wrote = true;
                versions.insert(key.clone(), written.expect("a put writes"));
                model.insert(key, value);
            }
            yabiir::Op::Delete { key } => {
                wrote |= model
                    .remove(&key)
                    .is_some();
                versions.remove(&key);
            }
            yabiir::Op::Check { .. } => {}
        }
    }
    assert_eq!(written.is_some(), wrote, "{written:?}");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn model_matches_reference(ops in prop::collection::vec(op_strategy(), 1..40)) {
        run_model(ops, Orders::Always(KeyOrder::Hashed));
    }

    #[test]
    fn model_matches_reference_with_a_sorted_keydir(ops in prop::collection::vec(op_strategy(), 1..40)) {
        run_model(ops, Orders::Always(KeyOrder::Sorted));
    }

    #[test]
    fn model_matches_reference_switching_order_at_every_reopen(ops in prop::collection::vec(op_strategy(), 1..40)) {
        run_model(ops, Orders::Alternating);
    }
}
