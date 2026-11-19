#!/usr/bin/env bash
# Rewrites the golden stores from their traces with the Rust engine (yabiir-diffd):
# <name>/ holds the files <name>.jsonl leaves, and <name>.expected what the store
# then holds, one "key value version" line per key in key order, in hex.
# Run it from anywhere; then check that GoldenTest still passes.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../../../../.." && pwd)
cargo build --quiet --release -p yabiir-difftest --manifest-path "$root/Cargo.toml"
diffd="$(cargo metadata --format-version 1 --no-deps --manifest-path "$root/Cargo.toml" | jq -r .target_directory)/release/yabiir-diffd"
for trace in "$here"/*.jsonl; do
    name=$(basename "$trace" .jsonl)
    dir="$here/$name"
    rm -rf "$dir"
    replies=$({
        sed "s|{DIR}|$dir|g" "$trace"
        echo '{"cmd":"open","dir":"'"$dir"'","max_file_size":4096,"sorted":true,"read_only":true}'
        echo '{"cmd":"scan_page","start":null,"end":null,"limit":1000000}'
        echo '{"cmd":"close"}'
    } | "$diffd")
    if grep -v '^{"ok":' <<<"$replies"; then
        echo "$name: the Rust engine replied with an error" >&2
        exit 1
    fi
    tail -n 2 <<<"$replies" | head -n 1 | jq -r '.ok[] | "\(.[0]) \(.[1]) \(.[2])"' >"$here/$name.expected"
    rm -f "$dir/.bitcask.lock"
done
