#!/bin/sh
# Fills a store through `sql`, whose table has the fields key, maker and year, and runs this
# chapter's statements. Then the bytes of one value, the insert race, and a million-row timing.
# Usage (from the repository root): book/code/ch04/run.sh <empty-scratch-dir>
set -e
D="$1/store"
mkdir -p "$D"
cargo build -q --release --manifest-path book/code/ch04/Cargo.toml
B=book/code/ch04/target/release
q() { echo "\$ sql DIR \"$1\"${2:+ $2}"; $B/sql "$D" "$1" $2; echo; }
q "INSERT INTO store VALUES ('apple-ii', 'apple', 1977)"
q "INSERT INTO store VALUES ('ibm-pc', 'ibm', 1981)"
q "INSERT INTO store VALUES ('bbc-micro', 'acorn', 1981)"
q "INSERT INTO store VALUES ('zx-spectrum', 'sinclair', 1982)"
q "INSERT INTO store VALUES ('c64', 'commodore', 1982)"
echo "\$ dump-value DIR zx-spectrum"; $B/dump-value "$D" zx-spectrum; echo
q "SELECT *"
q "SELECT key, year"
q "SELECT maker WHERE key = 'zx-spectrum'"
q "SELECT key, maker WHERE year = 1982"
q "SELECT key WHERE year BETWEEN 1978 AND 1981"
q "SELECT year"
q "SELECT DISTINCT year"
q "SELECT DISTINCT key, year"
q "INSERT INTO store VALUES ('c64', 'atari', 1999)"
q "SELECT * WHERE key = 'c64'"
q "INSERT INTO store VALUES ('amiga', 'commodore', 'nineteen eighty-five')"
q "INSERT INTO store VALUES ('amiga', 'commodore')"
q "SELECT price"
q "SELECT * WHERE year = '1982'"
q "INSERT INTO store VALUES ('amiga', 'commodore', 1985)"
q "SELECT DISTINCT maker"
echo "\$ insert-race DIR"; $B/insert-race "$1/race"; echo
echo "\$ time-where DIR"; $B/time-where "$1/time"
