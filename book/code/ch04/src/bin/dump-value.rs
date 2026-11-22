//! The bytes of one key's value, as the store holds them.
//!
//! usage: dump-value <store-dir> <key>

use yabiir::{Bitcask, Engine, Options};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dir, key] = args.as_slice() else {
        panic!("usage: dump-value <store-dir> <key>");
    };
    let store = Engine::open(dir, Options::default().read_only()).expect("open the store");
    let value = store.get(key.as_bytes()).expect("read").expect("no such key");
    for line in value.chunks(16) {
        let hex: Vec<String> = line.iter().map(|b| format!("{b:02x}")).collect();
        let text: String = line
            .iter()
            .map(|&b| if b.is_ascii_graphic() { b as char } else { '.' })
            .collect();
        println!("{:<47}  {text}", hex.join(" "));
    }
    println!("({} bytes)", value.len());
}
