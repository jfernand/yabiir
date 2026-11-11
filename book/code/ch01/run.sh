#!/bin/sh
# Writes one key, reads it, deletes it, and hexdumps every data file after each write.
# Each kv invocation reopens the store, which starts a new (empty) active file.
# Usage (from the repository root): book/code/ch01/run.sh <empty-scratch-dir>
set -e
KV=./target/release/kv
D="$1"
cargo build -q --release --bin kv
dump() { for f in "$D"/*.bitcask.data; do echo "\$ xxd $(basename "$f")  # $(wc -c < "$f") bytes"; xxd "$f"; done; }
echo '$ kv DIR add lang rust'; $KV "$D" add lang rust
dump
echo '$ kv DIR get lang';      $KV "$D" get lang
echo '$ kv DIR rm lang';       $KV "$D" rm lang
dump
