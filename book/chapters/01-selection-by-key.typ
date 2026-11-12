#import "../template/isss.typ": *

= Selection by Key <ch-keydir>

#status[Incipient. The query both ways, the entry format, the keydir and the measurement are in
place, all from real runs; recovery and the delete path are named but not walked through.]

On a store of a million rows, finding one key by visiting every row takes about 0.61 seconds. Asking
the store's index for it takes about a microsecond. Chapter 0's selection kept every row; this
chapter's is the first that drops rows, and the one a key/value store exists to answer fast:

```sql
SELECT * WHERE key = 'zx-spectrum'
```

== Selection with a condition

In the algebra this is a selection whose condition compares the key with a constant:

$ sigma_("key" = "zx-spectrum") ("store") = { t in "store" : t."key" = "zx-spectrum" } $

It needs nothing new from the API. Chapter 0's translation already had a condition, one that every
row satisfied; this one compares the key, and the `fold` keeps the rows for which it holds:#footnote[
#repo("book/code/ch01/src/lib.rs") holds both translations of this chapter;
#repo("book/code/ch01/src/main.rs") runs them against a store and prints the rows.]

```rust
let condition = |key: &[u8], _value: &[u8]| key == k;   // σ_key = k

store.fold(
    |key, value, (mut rows, visited): (Vec<Row>, usize)| {
        if condition(key, value) {
            rows.push((key.to_vec(), value.to_vec()));   // π_key,value
        }
        (rows, visited + 1)
    },
    (Vec::new(), 0),
)
```

Against Chapter 0's five computers it returns one row, and visits five:#footnote[All the runs in this
chapter are from #repo("book/code/ch01/run.sh"); its output is in
#repo("book/code/ch01/select-where.txt").]

#grid(columns: (1fr, 1fr), column-gutter: 10pt,
  output(title: "$ select-where DIR zx-spectrum --scan")[```
 key         | value
-------------+-------
 zx-spectrum | 1982
(1 row)
rows visited: 5
```],
  output(title: "$ select-where DIR amiga --scan")[```
 key | value
-----+-------
(0 rows)
rows visited: 5
```],
)

A key that isn't there costs the same: every row is visited to find that none matches. The answer
is right, and its price grows with the store.

== One row at most

No two records in a store share a key (@ch-select), so a condition that fixes the key can hold for
one row or for none. *A selection on the key returns at most one row, so it needs a lookup, not a
scan.* The API gains its third method, the one that does that lookup:

#srcfile("api.rs", lines: ((94, 96),))[```rust
    /// Retrieve the current value for `key`, or `Ok(None)` if the key does
    /// not exist (or has been deleted).
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>>;
```]

The translation becomes one call, and a missing key is an empty result instead of a scan that found
nothing:

```rust
store.get(k)?.map(|value| (k.to_vec(), value)).into_iter().collect()
```

#grid(columns: (1fr, 1fr), column-gutter: 10pt,
  output(title: "$ select-where DIR zx-spectrum --get")[```
 key         | value
-------------+-------
 zx-spectrum | 1982
(1 row)
```],
  output(title: "$ select-where DIR amiga --get")[```
 key | value
-----+-------
(0 rows)
```],
)

== The data file

`get` has to know where `zx-spectrum`'s value is on disk. A yabiir store is a directory of
append-only _data files_, the design Justin Sheehy and David Smith built at Basho for Riak, after Eric
Brewer pointed out to them that merging hash-table logs could be as fast as an LSM-tree, or faster
@sheehy2010bitcask:

- every `put` appends one _entry_ to the end of the _active_ data file;
- when the active file reaches its size limit, it's closed and a new one opens;
- `merge` copies the live entries of the closed files into new files and removes the old ones.

Nothing that has been written is modified in place. Here's the file that holds
`zx-spectrum`:#footnote[Each `kv` call opens the store, and opening always starts a new active file,
so the five rows, written by five calls, are in five files; this is file 3.]

#output(title: "00000000000000000003.bitcask.data · 31 bytes")[```
00000000: 8d0e 1935 4cf9 f46a 0b00 0000 0400 0000  ...5L..j........
00000010: 7a78 2d73 7065 6374 7275 6d31 3938 32    zx-spectrum1982
```]

Thirty-one bytes, every integer little-endian. `8d0e1935` is the CRC32 of the 27 bytes after it.
`4cf9f46a` is the timestamp: 1,794,439,500 seconds, 23:25 UTC on 11 November 2026. `0b000000` is the
key's length, 11, and `04000000` the value's, 4. Then the key, `zx-spectrum`, and the value, `1982`.
Sixteen bytes of header, fifteen of data, and the value starts at byte 16 + 11 = 27.

== The keydir

The _keydir_ is the store's index: an in-memory map from every live key to where its newest value is.
In Sheehy and Smith's words, "a keydir is simply a hash table" @sheehy2010bitcask, and in yabiir it's a
Rust `HashMap`:

#srcfile("keydir.rs", lines: ((12, 19), (45, 47)), notes: (
  (17, [For `zx-spectrum`: file 3, position 27, size 4.]),
))[```rust
pub struct KeydirEntry {
    pub file_id: u32,
    pub value_size: u32,
    /// Position of the VALUE bytes (past the entry header), not the entry
    /// header itself — this is exactly what a read seeks/preads from.
    pub value_pos: u64,
    pub timestamp: u32,
}
// …
pub struct Keydir {
    map: HashMap<Box<[u8]>, KeydirEntry, AHashState>,
}
```]

A `get` is one map lookup and one positioned read of `value_size` bytes at `value_pos`. For
`zx-spectrum` that's four bytes at position 27 of file 3: `1982`. The keydir is what `open` builds
before Chapter 0's `fold` can run, and the `fold` walks it.

#deep(title: "The files are the truth")[
  The keydir lives only in memory. Every `open` rebuilds it by reading the data files from the oldest
  to the newest, letting later entries replace earlier ones. A data file that has a _hint file_ —
  written by `merge`, holding each entry's key and position but not its value — is read through that
  instead.
]

#deep(title: "Finding the file is not enough")[
  Between the lookup and the read, a concurrent `merge` can move the value to a new file and remove
  the old one. Since #since("0.5.1") `get` takes the file's open handle in the same step as the
  lookup, under the keydir's lock; on unix an open handle keeps a removed file readable, so the read
  that follows still succeeds.
]

== Measured

A store of a million rows (keys `computer-0000000000` to `computer-0000999999`, four-byte values)
answered the query both ways, opened read-only, in a release build. _Scan_ is the median of five
`fold`s looking for one key near the middle; _get_ is the median and the 99th percentile of 100,000
`get`s for keys spread over the store. Three runs:#footnote[From
#repo("book/code/ch01/time.sh"), which runs #repo("book/code/ch01/src/bin/time-where.rs"); the output
is in #repo("book/code/ch01/time.txt").]

#table(
  columns: (auto, auto, auto, auto),
  [Run], [Scan, median], [Get, median], [Get, p99],
  [1], [614 ms], [0.98 µs], [1.56 µs],
  [2], [623 ms], [1.12 µs], [1.85 µs],
  [3], [612 ms], [0.98 µs], [1.47 µs],
)

The scan visits a million rows to keep one; the `get` reads four bytes. The ratio of the medians is
about 600,000. Both numbers are from memory: the store is 32 MB and sits in the page cache, so a
cold disk would make the scan slower still, and the `get` would pay for one read.

== Hash indexes

#block(sticky: true)[A hash map turns a key into a bucket number and goes straight to that bucket.
With _n_ rows in the store, that makes one kind of question cheap and every other kind a full walk:]

#table(
  columns: (auto, auto, 1fr),
  [Question], [Cost], [How],
  [`WHERE key = k`], [O(1)], [Hash `k`, go to its bucket.],
  [`WHERE value = v`], [O(n)], [Visit every row: the index is on the key.],
  [`WHERE key BETWEEN a AND b`], [O(n log n)], [Visit every row, sort, cut.],
  [`WHERE key LIKE 'zx-%'`], [O(n)], [Visit every row and test each key.],
)

None of Bitcask's eight design goals mention order — the first is "low latency per item read or
written" @sheehy2010bitcask — and the design meets that goal by giving order up.

#warn(title: "Hashing destroys order on purpose")[
  A good hash function sends similar keys to unrelated buckets. FxHash, tried for yabiir's keydir
  before aHash, does too little of that: 10,000 sequential keys of the form `key-0000000042` landed in
  one of 16,384 buckets, and every keydir operation became a walk of that one bucket.#footnote[The
  measurement is recorded in the doc comment on `Keydir`, in #repo("crates/yabiir/src/keydir.rs").]
  A hash that does spread keys well leaves no trace of their order, and only sorting gets it back.
]

That's selection by key: one lookup instead of a scan, because the key is unique and indexed. The
other rows of the table stay full walks; @ch-sorted makes the ranges cheap.

#takeaways(
  [`WHERE key = k` is a selection σ#sub[key = k]; a `fold` with that condition answers it, visiting
   every row.],
  [Keys are unique, so it returns at most one row, and `get` finds that row directly.],
  [A data file is a log of entries: header, key, value; nothing is modified in place.],
  [The keydir maps each live key to its file, position and size; `get` is one lookup and one read.],
  [On a million rows: about 0.61 s for the scan, about 1 µs for the `get`.],
  [A hash index answers only equality on the key; conditions on the value, ranges and prefixes are
   still full walks.],
)

#todo(
  [Recovery walked through on the same five files: how `open` rebuilds the keydir.],
  [A figure: data files on the left, keydir on the right, arrows for one `get`.],
  [The delete path, with its tombstone entry, when DML arrives (PLAN.md D2).],
)
