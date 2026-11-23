#!/bin/sh
# The order each key encoding gives, a composite key's bytes, prefix scans, and a hashed keydir
# timed against a sorted one.
# Usage (from the repository root): book/code/ch02/run.sh <empty-scratch-dir>
set -e
cargo build -q --release --manifest-path book/code/ch02/Cargo.toml
B=book/code/ch02/target/release
echo "\$ composite DIR"; $B/composite "$1/composite"; echo
echo "\$ keydir-times DIR"; $B/keydir-times "$1/times"
