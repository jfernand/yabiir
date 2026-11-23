#!/bin/sh
# A secondary index's cost and benefit, a crash at every byte of an insert, the race on an update,
# and an inverted index over yabiir's own source at v0.9.1.
# Usage (from the repository root): book/code/ch07/run.sh <empty-scratch-dir>
set -e
cargo build -q --release --manifest-path book/code/ch07/Cargo.toml
B=book/code/ch07/target/release
echo "\$ index-times DIR"; $B/index-times "$1/times"; echo
echo "\$ index-crash DIR"; mkdir -p "$1/crash"; $B/index-crash "$1/crash"; echo
echo "\$ index-race DIR"; mkdir -p "$1/race"; $B/index-race "$1/race"; echo
mkdir -p "$1/src"
git archive v0.9.1 crates/yabiir/src | tar -x -C "$1/src"
echo "\$ inverted DIR crates/yabiir/src tombstone version"
$B/inverted "$1/inverted" "$1/src/crates/yabiir/src" tombstone version
