#import "../template/isss.typ": *

= Selection by Key <ch-keydir>

#status[Incipient. The query both ways and the measurements are in place, all from real runs.]

On a store of a million rows, finding one key by visiting every row takes about 0.61 seconds. Asking
the store's index for it takes about a microsecond. Chapter 0's selection kept every row; this
chapter's is the first that drops rows, and the one a key/value store exists to answer fast:

```sql
SELECT * WHERE key = 'zx-spectrum'
```

#syntax(rr-seq(rr-kw("SELECT"), rr-opt(rr-kw("DISTINCT")),
  rr-choice(rr-kw("*"), rr-kw("key"), rr-kw("value")),
  rr-opt(rr-seq(rr-kw("WHERE", new: true), rr-kw("key", new: true), rr-kw("=", new: true),
    rr-ph("string", new: true)))))

The condition names the key field and compares it with a _string_, a constant in single quotes. With
no `FROM`, the table is always the store.

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
scan of the whole store.*#footnote[In the code, that scan is the call to `fold`, and the two words
can be read interchangeably: a scan visits every row once, and so does a `fold`, carrying its
accumulator along. _Scan_ is the database term; _fold_ is the idiomatic one in code, from functional
programming, where _scan_ means something else: a fold that also returns every intermediate
accumulator.] The API gains its third method, the one that does that lookup:

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

== Measurements of get and scan

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

== Hash indices in yabiir

#block(sticky: true)[`get` finds the row through the store's index, the _keydir_: an in-memory
hash map from every key to where its value is on disk. A hash map turns a key into a bucket number and goes straight to that bucket.
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
  measurement is recorded in the doc comment on `Keydir`, in #repo("crates/yabiir/src/keydir/mod.rs").]
  A hash that does spread keys well leaves no trace of their order, and only sorting gets it back.
]

That's selection by key: one lookup instead of a scan, because the key is unique and indexed. The
other rows of the table stay full walks; @ch-sorted makes the ranges cheap.

#takeaways(
  [`WHERE key = k` is a selection σ#sub[key = k]; a `fold` with that condition answers it, visiting
   every row.],
  [Keys are unique, so it returns at most one row, and `get` finds that row directly.],
  [On a million rows: about 0.61 s for the scan, about 1 µs for the `get`.],
  [A hash index answers only equality on the key; conditions on the value, ranges and prefixes are
   still full walks.],
)
