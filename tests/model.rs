//! Property-based model test (`docs/bitcask-implementation-plan.md` §10):
//! random sequences of put/delete/get/reopen/merge, checked against a plain
//! `HashMap` reference model. The highest-leverage test in the whole plan
//! for a storage engine like this — real bugs in a log-structured store
//! tend to show up only after specific *interleavings* of writes, deletes,
//! reopens and merges that a hand-written test case is unlikely to think
//! to try, but a few hundred random ones very likely will.
//!
//! Black-box: only the public `yabiir` API is used, exactly as any real
//! caller would use it.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use proptest::prelude::*;
use yabiir::{now_unix, Bitcask, Engine, Options};

/// Minimal self-cleaning temp directory — same pattern used throughout this
/// crate's own tests and benches.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "yabiir-model-test-{}-{}-{}",
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

#[derive(Debug, Clone)]
enum Op {
    Put(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
    Get(Vec<u8>),
    Reopen,
    Merge,
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
    ]
}

/// Small `max_file_size` so a typical op sequence actually exercises
/// rotation (and gives merge non-trivial input) instead of everything
/// landing in one never-rotated active file.
fn small_file_options() -> Options {
    Options {
        max_file_size: 200,
        ..Options::default()
    }
}

fn run_model(ops: Vec<Op>) {
    let dir = TempDir::new();
    let mut model: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();
    let mut db = Engine::open(&*dir, small_file_options()).unwrap();

    for op in ops {
        match op {
            Op::Put(k, v) => {
                db.put(&k, &v, now_unix()).unwrap();
                model.insert(k, v);
            }
            Op::Delete(k) => {
                db.delete(&k, now_unix()).unwrap();
                model.remove(&k);
            }
            Op::Get(k) => {
                assert_eq!(db.get(&k).unwrap(), model.get(&k).cloned());
            }
            Op::Reopen => {
                drop(db);
                db = Engine::open(&*dir, small_file_options()).unwrap();
            }
            Op::Merge => {
                db.merge().unwrap();
            }
        }
    }

    // Final full comparison, not just the interleaved Get checks above —
    // catches anything a Get happened not to probe during the sequence.
    let mut keys: Vec<_> = db.list_keys().unwrap();
    keys.sort();
    let mut model_keys: Vec<_> = model.keys().cloned().collect();
    model_keys.sort();
    assert_eq!(keys, model_keys);
    for k in &keys {
        assert_eq!(db.get(k).unwrap().as_ref(), model.get(k));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn model_matches_reference(ops in prop::collection::vec(op_strategy(), 1..40)) {
        run_model(ops);
    }
}
