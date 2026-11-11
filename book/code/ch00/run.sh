#!/bin/sh
# Fills a store (key: a home computer, value: the year it went on sale) with yabiir's `kv`
# tool, then runs select-star — the hand translation of `SELECT *` (both fields, every row)
# into open + fold — twice, reopening the store each time. yabiir has no SQL.
# Usage (from the repository root): book/code/ch00/run.sh <empty-scratch-dir>
set -e
D="$1"
cargo build -q --release --bin kv
cargo build -q --release --manifest-path book/code/ch00/Cargo.toml
KV=./target/release/kv
SELECT=book/code/ch00/target/release/select-star
$KV "$D" add apple-ii    1977
$KV "$D" add ibm-pc      1981
$KV "$D" add bbc-micro   1981
$KV "$D" add zx-spectrum 1982
$KV "$D" add c64         1982
echo '$ select-star DIR'; $SELECT "$D"
echo '$ select-star DIR'; $SELECT "$D"
