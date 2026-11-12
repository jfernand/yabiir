#!/bin/sh
# Fills a store with the five computers of Chapter 0 using yabiir's `kv` tool (one
# `kv` call per row, and each call starts a new data file), then runs the hand
# translation of `SELECT * WHERE key = k` both ways, for a key that exists and one
# that doesn't, and hexdumps the data file holding `zx-spectrum`.
# Usage (from the repository root): book/code/ch01/run.sh <empty-scratch-dir>
set -e
D="$1"
cargo build -q --release --bin kv
cargo build -q --release --manifest-path book/code/ch01/Cargo.toml
KV=./target/release/kv
SW=book/code/ch01/target/release/select-where
$KV "$D" add apple-ii    1977
$KV "$D" add ibm-pc      1981
$KV "$D" add bbc-micro   1981
$KV "$D" add zx-spectrum 1982
$KV "$D" add c64         1982
for k in zx-spectrum amiga; do
  for m in --scan --get; do
    echo "\$ select-where DIR $k $m"; $SW "$D" "$k" "$m"
  done
done
F=$(grep -l zx-spectrum "$D"/*.bitcask.data)
echo "\$ xxd $(basename "$F")  # $(wc -c < "$F") bytes"; xxd "$F"
