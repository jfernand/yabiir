//! What each way of writing a key does to its order, the bytes of one
//! composite key, and prefix scans over a store of them.
//!
//! usage: composite <empty-dir>

use std::ops::Bound;

use yabiir::{Bitcask, Engine, KeyOrder, Options};
use yabiir_sql::key::{decode_key, encode_key, prefix_range};
use yabiir_sql::table::Datum;

fn text(s: &str) -> Datum {
    Datum::Text(s.into())
}

fn show(parts: &[Datum]) -> String {
    let inner: Vec<String> = parts
        .iter()
        .map(|d| match d {
            Datum::Integer(n) => n.to_string(),
            Datum::Text(s) => format!("{s:?}"),
        })
        .collect();
    format!("({})", inner.join(", "))
}

/// `values`, sorted by the bytes `encode` gives them.
fn sorted_by(values: &[i64], encode: impl Fn(i64) -> Vec<u8>) -> String {
    let mut v = values.to_vec();
    v.sort_by_key(|&n| encode(n));
    v.iter().map(ToString::to_string).collect::<Vec<_>>().join(" ")
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: composite <empty-dir>");

    let numbers = [-1, 7, 42, 100, 256];
    println!("integers, sorted by their bytes");
    println!("  as text          {}", sorted_by(&numbers, |n| n.to_string().into_bytes()));
    println!("  little-endian    {}", sorted_by(&numbers, |n| n.to_le_bytes().to_vec()));
    println!("  big-endian       {}", sorted_by(&numbers, |n| n.to_be_bytes().to_vec()));
    println!("  sign bit flipped {}", sorted_by(&numbers, |n| encode_key(&[Datum::Integer(n)])));
    println!();

    let key = [text("alice"), text("INBOX"), Datum::Integer(42)];
    let bytes = encode_key(&key);
    println!("{}", show(&key));
    for line in bytes.chunks(16) {
        let hex: Vec<String> = line.iter().map(|b| format!("{b:02x}")).collect();
        let shown: String =
            line.iter().map(|&b| if b.is_ascii_graphic() { b as char } else { '.' }).collect();
        println!("{:<47}  {shown}", hex.join(" "));
    }
    println!("({} bytes)", bytes.len());
    assert_eq!(decode_key(&bytes).unwrap(), key);
    println!();

    let mut names = vec![
        vec![text("al"), text("z")],
        vec![text("al\0x"), text("a")],
        vec![text("alice"), text("a")],
    ];
    names.sort_by_key(|k| encode_key(k));
    println!("strings, sorted by their encoded bytes");
    for k in &names {
        println!("  {}", show(k));
    }
    let glued = |k: &[Datum]| -> Vec<u8> {
        k.iter()
            .flat_map(|d| match d {
                Datum::Text(s) => s.as_bytes().to_vec(),
                Datum::Integer(_) => unreachable!(),
            })
            .collect()
    };
    println!(
        "  without terminators, (\"al\", \"ice\") and (\"alice\", \"\") are the same key: {}",
        glued(&[text("al"), text("ice")]) == glued(&[text("alice"), text("")])
    );
    println!();

    let options = Options { key_order: KeyOrder::Sorted, ..Options::default() };
    let store = Engine::open(&dir, options).unwrap();
    for (account, mailbox, uid) in [
        ("al", "INBOX", 1),
        ("alice", "INBOX", 41),
        ("alice", "INBOX", 42),
        ("alice", "INBOX", 100),
        ("alice", "Sent", 7),
        ("bob", "INBOX", 3),
    ] {
        let k = encode_key(&[text(account), text(mailbox), Datum::Integer(uid)]);
        store.put(&k, b"", yabiir::now_unix()).unwrap();
    }
    for prefix in [vec![text("alice")], vec![text("alice"), text("INBOX")]] {
        let (low, high) = prefix_range(&encode_key(&prefix));
        let low = match &low {
            Bound::Included(k) => Bound::Included(k.as_slice()),
            _ => unreachable!(),
        };
        let high = match &high {
            Bound::Excluded(k) => Bound::Excluded(k.as_slice()),
            _ => unreachable!(),
        };
        println!("scan of the prefix {}", show(&prefix));
        for row in store.scan(low, high).unwrap() {
            println!("  {}", show(&decode_key(&row.0).unwrap()));
        }
    }
}
