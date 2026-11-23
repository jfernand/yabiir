#!/bin/sh
# The oldest-computer query of Appendix B, from the calculus and from the algebra.
# Usage (from the repository root): book/code/appB/run.sh <empty-scratch-dir>
set -e
cargo build -q --release --manifest-path book/code/appB/Cargo.toml
echo "\$ oldest DIR"; book/code/appB/target/release/oldest "$1/store"
