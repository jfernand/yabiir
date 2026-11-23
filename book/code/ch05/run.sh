#!/bin/sh
# Fills a store through `sql` and runs this chapter's DELETEs: the bytes of a tombstone, a crash
# at every byte of a multi-row delete, the race with a concurrent write, and the space on disk.
# Usage (from the repository root): book/code/ch05/run.sh <empty-scratch-dir>
set -e
D="$1/store"
mkdir -p "$D"
cargo build -q --release --manifest-path book/code/ch05/Cargo.toml
B=book/code/ch05/target/release
q() { echo "\$ sql DIR \"$1\"${2:+ $2}"; $B/sql "$D" "$1" $2; echo; }
q "INSERT INTO store VALUES ('apple-ii', 'apple', 1977)"
q "INSERT INTO store VALUES ('ibm-pc', 'ibm', 1981)"
q "INSERT INTO store VALUES ('bbc-micro', 'acorn', 1981)"
q "INSERT INTO store VALUES ('zx-spectrum', 'sinclair', 1982)"
q "INSERT INTO store VALUES ('c64', 'commodore', 1982)"
q "DELETE FROM store WHERE key = 'c64'"
echo "\$ dump-tail DIR 35"; $B/dump-tail "$D" 35; echo
q "SELECT * WHERE key = 'c64'"
q "DELETE FROM store WHERE key = 'c64'"
q "INSERT INTO store VALUES ('c64', 'commodore', 1982)"
q "DELETE FROM store WHERE year = 1982"
q "SELECT *"
q "DELETE FROM store WHERE year = '1981'"
q "DELETE FROM computers"
q "DELETE FROM store WHERE maker BETWEEN 'a' AND 'b'"
q "SELECT *"
q "DELETE FROM store"
q "SELECT *"
echo "\$ delete-crash DIR"; $B/delete-crash "$1/crash"; echo
echo "\$ delete-race DIR"; $B/delete-race "$1/race"; echo
echo "\$ space DIR"; $B/space "$1/space"
