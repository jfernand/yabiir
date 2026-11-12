#!/bin/sh
# Times `SELECT * WHERE key = k` both ways on a store of a million rows (see
# src/bin/time-where.rs for what is measured), on a release build.
# Usage (from the repository root): book/code/ch01/time.sh <empty-scratch-dir>
set -e
cargo build -q --release --manifest-path book/code/ch01/Cargo.toml
book/code/ch01/target/release/time-where "$1" 1000000
