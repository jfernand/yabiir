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

fn page_keys(page: &[(Vec<u8>, crate::Versioned)]) -> Vec<&str> {
    page.iter()
        .map(|(k, _)| std::str::from_utf8(k).unwrap())
        .collect()
}

/// Every key between `start` and `end`, a page of `limit` at a time.
fn all_pages(
    db: &Engine,
    start: Bound<&[u8]>,
    end: Bound<&[u8]>,
    direction: crate::Direction,
    limit: usize,
) -> Vec<Vec<String>> {
    let mut pages = Vec::new();
    let mut after: Option<Vec<u8>> = None;
    loop {
        let (start, end) = match (&after, direction) {
            (None, _) => (start, end),
            (Some(a), crate::Direction::Forward) => (Bound::Excluded(a.as_slice()), end),
            (Some(a), crate::Direction::Reverse) => (start, Bound::Excluded(a.as_slice())),
        };
        let page = db
            .scan_page(start, end, direction, limit)
            .unwrap();
        let is_last = page.len() < limit;
        after = page
            .last()
            .map(|(k, _)| k.clone());
        pages.push(
            page_keys(&page)
                .into_iter()
                .map(String::from)
                .collect(),
        );
        if is_last {
            return pages;
        }
    }
}

#[test]
fn scan_page_pages_through_a_range_forward_and_in_reverse() {
    let dir = TempDir::new();
    let db = Engine::open(&*dir, sorted_options()).unwrap();
    computers(&db);
    let (b, z): (&[u8], &[u8]) = (b"b", b"z");

    assert_eq!(
        all_pages(
            &db,
            Bound::Included(b),
            Bound::Excluded(z),
            crate::Direction::Forward,
            2
        ),
        vec![vec!["bbc-micro", "c64"], vec!["ibm-pc"]]
    );
    assert_eq!(
        all_pages(
            &db,
            Bound::Unbounded,
            Bound::Unbounded,
            crate::Direction::Reverse,
            2
        ),
        vec![
            vec!["zx-spectrum", "ibm-pc"],
            vec!["c64", "bbc-micro"],
            vec!["apple-ii"]
        ]
    );
    // An exact multiple of the limit ends with an empty page.
    assert_eq!(
        all_pages(
            &db,
            Bound::Unbounded,
            Bound::Excluded(b"d"),
            crate::Direction::Forward,
            3
        ),
        vec![vec!["apple-ii", "bbc-micro", "c64"], vec![]]
    );
}

#[test]
fn scan_page_returns_values_and_versions() {
    let dir = TempDir::new();
    let db = Engine::open(&*dir, sorted_options()).unwrap();
    computers(&db);
    db.put(b"c64", b"1982!", now_unix())
        .unwrap();
    let page = db
        .scan_page(
            Bound::Included(b"c64"),
            Bound::Included(b"c64"),
            crate::Direction::Forward,
            10,
        )
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(
        page[0].1,
        db.get_versioned(b"c64")
            .unwrap()
            .unwrap()
    );
    assert_eq!(
        page[0]
            .1
            .value,
        b"1982!"
    );
}

#[test]
fn scan_page_needs_a_sorted_keydir_and_honours_a_zero_limit() {
    let dir = TempDir::new();
    let db = open(&dir);
    assert!(matches!(
        db.scan_page(
            Bound::Unbounded,
            Bound::Unbounded,
            crate::Direction::Forward,
            1
        ),
        Err(Error::Unordered)
    ));
    drop(db);
    let db = Engine::open(&*dir, sorted_options()).unwrap();
    computers(&db);
    assert!(
        db.scan_page(
            Bound::Unbounded,
            Bound::Unbounded,
            crate::Direction::Reverse,
            0
        )
        .unwrap()
        .is_empty()
    );
}
