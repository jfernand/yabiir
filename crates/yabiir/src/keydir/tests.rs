use super::*;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn entry(n: u32) -> KeydirEntry {
    KeydirEntry {
        file_id: n,
        value_size: n,
        value_pos: n as u64,
        timestamp: n,
    }
}

fn sorted_with(keys: &[&str]) -> Keydir {
    let mut kd = Keydir::with_order(KeyOrder::Sorted);
    for (i, k) in keys
        .iter()
        .enumerate()
    {
        kd.insert(k.as_bytes(), entry(i as u32));
    }
    kd
}

fn keys_of<'a>(it: impl Iterator<Item = (&'a [u8], &'a KeydirEntry)>) -> Vec<&'a [u8]> {
    it.map(|(k, _)| k)
        .collect()
}

#[test]
fn both_orders_agree_on_every_point_operation() {
    for order in [KeyOrder::Hashed, KeyOrder::Sorted] {
        let mut kd = Keydir::with_order(order);
        assert_eq!(kd.order(), order);
        kd.insert(b"a", entry(1));
        kd.insert(b"b", entry(2));
        kd.insert(b"a", entry(3)); // overwrite
        assert_eq!(kd.get(b"a"), Some(entry(3)), "{order:?}");
        assert_eq!(kd.len(), 2, "{order:?}");
        assert!(!kd.cas_repoint(b"a", entry(1), entry(9)), "{order:?}");
        assert!(kd.cas_repoint(b"a", entry(3), entry(9)), "{order:?}");
        assert_eq!(kd.get(b"a"), Some(entry(9)), "{order:?}");
        assert_eq!(kd.remove(b"b"), Some(entry(2)), "{order:?}");
        assert_eq!(kd.get(b"b"), None, "{order:?}");
        assert_eq!(kd.len(), 1, "{order:?}");
    }
}

#[test]
fn a_sorted_keydir_iterates_in_key_order() {
    let kd = sorted_with(&["c64", "apple-ii", "zx-spectrum", "bbc-micro", "ibm-pc"]);
    assert_eq!(
        keys_of(kd.iter()),
        vec![
            &b"apple-ii"[..],
            b"bbc-micro",
            b"c64",
            b"ibm-pc",
            b"zx-spectrum"
        ]
    );
}

#[test]
fn range_honours_each_kind_of_bound_in_key_order() {
    let kd = sorted_with(&["a", "b", "c", "d", "e"]);
    let r = |s: Bound<&[u8]>, e: Bound<&[u8]>| {
        keys_of(
            kd.range(s, e)
                .unwrap(),
        )
    };
    use Bound::{Excluded, Included, Unbounded};
    assert_eq!(
        r(Included(b"b"), Included(b"d")),
        vec![&b"b"[..], b"c", b"d"]
    );
    assert_eq!(r(Included(b"b"), Excluded(b"d")), vec![&b"b"[..], b"c"]);
    assert_eq!(r(Excluded(b"b"), Included(b"d")), vec![&b"c"[..], b"d"]);
    assert_eq!(r(Unbounded, Excluded(b"c")), vec![&b"a"[..], b"b"]);
    assert_eq!(r(Included(b"d"), Unbounded), vec![&b"d"[..], b"e"]);
    // Bounds between stored keys still select the keys between them.
    assert_eq!(r(Included(b"bb"), Included(b"dd")), vec![&b"c"[..], b"d"]);
}

#[test]
fn range_with_bounds_that_select_nothing_is_empty_rather_than_a_panic() {
    // BTreeMap::range itself panics on these.
    let kd = sorted_with(&["a", "b", "c"]);
    use Bound::{Excluded, Included};
    for (s, e) in [
        (Included(&b"c"[..]), Included(&b"a"[..])),
        (Excluded(&b"b"[..]), Excluded(&b"b"[..])),
        (Included(&b"b"[..]), Excluded(&b"b"[..])),
        (Excluded(&b"b"[..]), Included(&b"b"[..])),
    ] {
        assert_eq!(
            kd.range(s, e)
                .unwrap()
                .count(),
            0
        );
    }
    assert_eq!(
        kd.range(Included(b"b"), Included(b"b"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn a_hashed_keydir_has_no_range() {
    let mut kd = Keydir::new();
    kd.insert(b"a", entry(1));
    assert!(
        kd.range(Bound::Unbounded, Bound::Unbounded)
            .is_none()
    );
}

#[test]
fn insert_then_get_returns_same_entry() {
    let mut kd = Keydir::new();
    kd.insert(b"k", entry(1));
    assert_eq!(kd.get(b"k"), Some(entry(1)));
}

#[test]
fn insert_twice_is_last_write_wins() {
    let mut kd = Keydir::new();
    kd.insert(b"k", entry(1));
    kd.insert(b"k", entry(2));
    assert_eq!(kd.get(b"k"), Some(entry(2)));
    assert_eq!(kd.len(), 1);
}

#[test]
fn remove_then_get_returns_none() {
    let mut kd = Keydir::new();
    kd.insert(b"k", entry(1));
    assert_eq!(kd.remove(b"k"), Some(entry(1)));
    assert_eq!(kd.get(b"k"), None);
    assert!(kd.is_empty());
}

#[test]
fn remove_missing_key_returns_none() {
    let mut kd = Keydir::new();
    assert_eq!(kd.remove(b"missing"), None);
}

#[test]
fn cas_repoint_succeeds_when_expectation_matches() {
    let mut kd = Keydir::new();
    kd.insert(b"k", entry(1));
    assert!(kd.cas_repoint(b"k", entry(1), entry(2)));
    assert_eq!(kd.get(b"k"), Some(entry(2)));
}

#[test]
fn cas_repoint_fails_when_expectation_is_stale() {
    let mut kd = Keydir::new();
    kd.insert(b"k", entry(1));
    kd.insert(b"k", entry(2)); // a "concurrent write" raced ahead
    assert!(!kd.cas_repoint(b"k", entry(1), entry(3)));
    // the stale CAS must not have mutated anything
    assert_eq!(kd.get(b"k"), Some(entry(2)));
}

#[test]
fn cas_repoint_fails_when_key_is_absent() {
    let mut kd = Keydir::new();
    assert!(!kd.cas_repoint(b"missing", entry(1), entry(2)));
    assert_eq!(kd.get(b"missing"), None);
}

#[test]
fn iter_yields_all_entries() {
    let mut kd = Keydir::new();
    kd.insert(b"a", entry(1));
    kd.insert(b"b", entry(2));
    let mut seen: Vec<_> = kd
        .iter()
        .map(|(k, v)| (k.to_vec(), *v))
        .collect();
    seen.sort_by(|a, b| {
        a.0.cmp(&b.0)
    });
    assert_eq!(
        seen,
        vec![(b"a".to_vec(), entry(1)), (b"b".to_vec(), entry(2))]
    );
}

#[test]
fn snapshot_matches_iter() {
    let mut kd = Keydir::new();
    kd.insert(b"a", entry(1));
    kd.insert(b"b", entry(2));
    let shared = SharedKeydir::new(kd);

    let mut snap = shared.snapshot();
    snap.sort_by(|a, b| {
        a.0.cmp(&b.0)
    });
    assert_eq!(
        snap,
        vec![
            (Box::<[u8]>::from(b"a".as_slice()), entry(1)),
            (Box::<[u8]>::from(b"b".as_slice()), entry(2)),
        ]
    );
}

/// N reader threads calling `get` in a loop while 1 writer thread
/// inserts/removes the same key for a fixed duration. Every value a
/// reader observes must be exactly the sentinel entry the writer always
/// uses — never a torn/partial/garbage struct — and nothing should
/// panic or deadlock.
#[test]
fn concurrent_reads_and_writes_stay_consistent() {
    let keydir = Arc::new(SharedKeydir::new(Keydir::new()));
    let key: &'static [u8] = b"shared-key";
    let sentinel = entry(42);
    let deadline = Instant::now() + Duration::from_millis(200);

    let writer = {
        let keydir = Arc::clone(&keydir);
        thread::spawn(move || {
            while Instant::now() < deadline {
                keydir.insert(key, sentinel);
                keydir.remove(key);
            }
        })
    };

    let readers: Vec<_> = (0..4)
        .map(|_| {
            let keydir = Arc::clone(&keydir);
            thread::spawn(move || {
                while Instant::now() < deadline {
                    if let Some(observed) = keydir.get(key) {
                        assert_eq!(observed, sentinel);
                    }
                }
            })
        })
        .collect();

    writer
        .join()
        .unwrap();
    for reader in readers {
        reader
            .join()
            .unwrap();
    }
}

#[test]
fn shared_keydir_len_and_is_empty_reflect_contents() {
    let shared = SharedKeydir::new(Keydir::new());
    assert_eq!(shared.len(), 0);
    assert!(shared.is_empty());

    shared.insert(b"k", entry(1));
    assert_eq!(shared.len(), 1);
    assert!(!shared.is_empty());

    shared.remove(b"k");
    assert_eq!(shared.len(), 0);
    assert!(shared.is_empty());
}
