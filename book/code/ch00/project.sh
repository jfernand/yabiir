#!/bin/sh
# Fills a store (key: a home computer, value: the year it went on sale) with yabiir's `kv`
# tool, then runs select-project — the hand translation of `SELECT key`, `SELECT value` and
# `SELECT DISTINCT value` into open + fold. yabiir has no SQL.
# Usage (from the repository root): book/code/ch00/project.sh <empty-scratch-dir>
set -e
D="$1"
cargo build -q --release --bin kv
cargo build -q --release --manifest-path book/code/ch00/Cargo.toml
KV=./target/release/kv
PROJECT=book/code/ch00/target/release/select-project
$KV "$D" add apple-ii    1977
$KV "$D" add ibm-pc      1981
$KV "$D" add bbc-micro   1981
$KV "$D" add zx-spectrum 1982
$KV "$D" add c64         1982
echo '$ select-project DIR key'; $PROJECT "$D" key
echo '$ select-project DIR value'; $PROJECT "$D" value
echo '$ select-project DIR value --distinct'; $PROJECT "$D" value --distinct
