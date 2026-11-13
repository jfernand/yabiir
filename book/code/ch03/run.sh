#!/bin/sh
# Fills a store (key: a home computer, value: the year it went on sale) with yabiir's `kv`
# tool, then runs queries through `sql`, which parses each one, plans it and runs the plan,
# printing the tree, the plan and the rows. The store is opened read-only, hashed unless
# `--sorted` is given.
# Usage (from the repository root): book/code/ch03/run.sh <empty-scratch-dir>
set -e
D="$1"
cargo build -q --release --bin kv
cargo build -q --release --manifest-path book/code/ch03/Cargo.toml
KV=./target/release/kv
SQL=book/code/ch03/target/release/sql
$KV "$D" add apple-ii    1977
$KV "$D" add ibm-pc      1981
$KV "$D" add bbc-micro   1981
$KV "$D" add zx-spectrum 1982
$KV "$D" add c64         1982
q() { echo "\$ sql DIR \"$1\"${2:+ $2}"; $SQL "$D" "$1" $2; echo; }
q "SELECT *"
q "SELECT DISTINCT value"
q "SELECT DISTINCT key"
q "SELECT value WHERE key = 'zx-spectrum'"
q "SELECT DISTINCT value WHERE key = 'zx-spectrum'"
q "SELECT * WHERE key BETWEEN 'a' AND 'c'"
q "SELECT * WHERE key BETWEEN 'a' AND 'c'" --sorted
q "SELECT * WHERE value = '1982'"
