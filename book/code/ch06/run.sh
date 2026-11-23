#!/bin/sh
# Fills a store through `sql` and runs this chapter's UPDATEs, then the race between concurrent
# increments and a crash at every byte of a change of key.
# Usage (from the repository root): book/code/ch06/run.sh <empty-scratch-dir>
set -e
D="$1/store"
mkdir -p "$D"
cargo build -q --release --manifest-path book/code/ch06/Cargo.toml
B=book/code/ch06/target/release
q() { echo "\$ sql DIR \"$1\"${2:+ $2}"; $B/sql "$D" "$1" $2; echo; }
q "INSERT INTO store VALUES ('apple-ii', 'apple', 1977)"
q "INSERT INTO store VALUES ('ibm-pc', 'ibm', 1981)"
q "INSERT INTO store VALUES ('bbc-micro', 'acorn', 1981)"
q "INSERT INTO store VALUES ('zx-spectrum', 'sinclair', 1982)"
q "INSERT INTO store VALUES ('c64', 'commodore', 1982)"
q "UPDATE store SET maker = 'cbm' WHERE key = 'c64'"
q "SELECT * WHERE key = 'c64'"
q "UPDATE store SET year = year + 1 WHERE year = 1981"
q "SELECT key, year WHERE year = 1982"
q "UPDATE store SET year = year + 1, maker = 'clive' WHERE key = 'zx-spectrum'"
q "SELECT * WHERE key = 'zx-spectrum'"
q "UPDATE store SET year = 'soon'"
q "UPDATE store SET maker = maker + 1"
q "UPDATE store SET year = 1, year = 2"
q "UPDATE store SET key = 'c128' WHERE key = 'c64'"
q "SELECT * WHERE key = 'c128'"
q "SELECT * WHERE key = 'c64'"
q "UPDATE store SET key = 'ibm-pc' WHERE key = 'c128'"
q "UPDATE store SET key = 'same' WHERE year = 1982"
echo "\$ update-race DIR"; $B/update-race "$1/race"; echo
echo "\$ move-crash DIR"; $B/move-crash "$1/crash"
