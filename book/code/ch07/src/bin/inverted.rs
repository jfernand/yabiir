//! An inverted index over a directory of text files, in one sorted store:
//! one key per (word, document). `AND` walks two postings lists at once.
//!
//! usage: inverted <empty-dir> <source-dir> <word> <word>

use std::collections::BTreeSet;
use std::fs;
use std::ops::Bound;
use std::path::{Path, PathBuf};

use indexes::open;
use yabiir::Bitcask;
use yabiir_sql::key::{decode_key, encode_key, prefix_range};
use yabiir_sql::table::Datum;

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(str::to_ascii_lowercase)
        .collect()
}

/// The sorted document numbers of a word's postings.
fn postings(store: &yabiir::Engine, word: &str) -> Vec<i64> {
    let (low, high) = prefix_range(&encode_key(&[Datum::Text(word.into())]));
    let (Bound::Included(low), Bound::Excluded(high)) = (low, high) else { unreachable!() };
    store
        .scan(Bound::Included(&low), Bound::Excluded(&high))
        .unwrap()
        .into_iter()
        .map(|(key, _)| match decode_key(&key).unwrap()[1] {
            Datum::Integer(doc) => doc,
            _ => unreachable!(),
        })
        .collect()
}

/// `AND`: two sorted lists, walked together.
fn intersect(a: &[i64], b: &[i64]) -> (Vec<i64>, usize) {
    let (mut i, mut j, mut steps) = (0, 0, 0);
    let mut out = Vec::new();
    while i < a.len() && j < b.len() {
        steps += 1;
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    (out, steps)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dir, source, first, second] = args.as_slice() else {
        panic!("usage: inverted <empty-dir> <source-dir> <word> <word>");
    };
    let store = open(Path::new(dir));
    let mut paths = Vec::new();
    files(Path::new(source), &mut paths);
    paths.sort();
    let mut entries = 0;
    let mut vocabulary = BTreeSet::new();
    for (doc, path) in paths.iter().enumerate() {
        for word in words(&fs::read_to_string(path).unwrap()) {
            let key = encode_key(&[Datum::Text(word.clone()), Datum::Integer(doc as i64)]);
            store.put(&key, b"", 1).unwrap();
            vocabulary.insert(word);
            entries += 1;
        }
    }
    println!("{} files, {} words, {entries} postings", paths.len(), vocabulary.len());
    let (a, b) = (postings(&store, first), postings(&store, second));
    let (both, steps) = intersect(&a, &b);
    println!("{first}: {} files", a.len());
    println!("{second}: {} files", b.len());
    println!("{first} AND {second}: {} files, in {steps} steps", both.len());
    for doc in both {
        let name = paths[doc as usize].strip_prefix(source).unwrap();
        println!("  {}", name.display());
    }
}
