use super::*;
use std::ops::Bound;

fn sorted_options() -> Options {
    Options {
        key_order: crate::KeyOrder::Sorted,
        ..Options::default()
    }
}

fn computers(db: &Engine) {
    for (k, v) in [
        ("c64", "1982"),
        ("apple-ii", "1977"),
        ("zx-spectrum", "1982"),
        ("bbc-micro", "1981"),
        ("ibm-pc", "1981"),
    ] {
        db.put(k.as_bytes(), v.as_bytes(), now_unix())
            .unwrap();
    }
}

fn keys_of(rows: &[(Vec<u8>, Vec<u8>)]) -> Vec<&str> {
    rows.iter()
        .map(|(k, _)| std::str::from_utf8(k).unwrap())
        .collect()
}

#[test]
fn scan_returns_the_keys_in_range_in_order_with_their_values() {
    let dir = TempDir::new();
    let db = Engine::open(
        &*dir,
        Options {
            max_file_size: 1, // every put in its own file
            ..sorted_options()
        },
    )
    .unwrap();
    computers(&db);

    let rows = db
        .scan(Bound::Included(b"b"), Bound::Excluded(b"j"))
        .unwrap();
    assert_eq!(keys_of(&rows), vec!["bbc-micro", "c64", "ibm-pc"]);
    assert_eq!(rows[1].1, b"1982");

    // After a merge has moved every value into new files, the same answer.
    db.merge()
        .unwrap();
    assert_eq!(
        db.scan(Bound::Included(b"b"), Bound::Excluded(b"j"))
            .unwrap(),
        rows
    );
    // And list_keys follows the keydir's order too.
    assert_eq!(
        db.list_keys()
            .unwrap(),
        vec![
            b"apple-ii".to_vec(),
            b"bbc-micro".to_vec(),
            b"c64".to_vec(),
            b"ibm-pc".to_vec(),
            b"zx-spectrum".to_vec()
        ]
    );
}

#[test]
fn scan_on_a_hashed_store_is_an_unordered_error() {
    let dir = TempDir::new();
    let db = open(&dir); // Options::default(): KeyOrder::Hashed
    computers(&db);
    assert!(matches!(
        db.scan(Bound::Unbounded, Bound::Unbounded),
        Err(Error::Unordered)
    ));
}

#[test]
fn the_same_store_opens_with_either_order() {
    let dir = TempDir::new();
    let hashed = open(&dir);
    computers(&hashed);
    drop(hashed);

    let sorted = Engine::open(&*dir, sorted_options()).unwrap();
    assert_eq!(
        keys_of(
            &sorted
                .scan(Bound::Unbounded, Bound::Unbounded)
                .unwrap()
        ),
        vec!["apple-ii", "bbc-micro", "c64", "ibm-pc", "zx-spectrum"]
    );
    sorted
        .put(b"amiga", b"1985", now_unix())
        .unwrap();
    drop(sorted);

    let hashed = open(&dir);
    assert_eq!(
        hashed
            .get(b"amiga")
            .unwrap(),
        Some(b"1985".to_vec())
    );
    assert_eq!(
        hashed
            .list_keys()
            .unwrap()
            .len(),
        6
    );
}
