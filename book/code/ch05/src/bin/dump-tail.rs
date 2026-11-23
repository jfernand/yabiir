//! The last bytes of a store's newest data file.
//!
//! usage: dump-tail <store-dir> <bytes>

use std::fs;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dir, n] = args.as_slice() else {
        panic!("usage: dump-tail <store-dir> <bytes>");
    };
    let n: usize = n.parse().expect("a number of bytes");
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".bitcask.data"))
        .collect();
    files.sort();
    let bytes = fs::read(files.last().expect("a data file")).unwrap();
    for line in bytes[bytes.len() - n..].chunks(16) {
        let hex: Vec<String> = line.iter().map(|b| format!("{b:02x}")).collect();
        let text: String = line
            .iter()
            .map(|&b| if b.is_ascii_graphic() { b as char } else { '.' })
            .collect();
        println!("{:<47}  {text}", hex.join(" "));
    }
    println!("({n} bytes)");
}
