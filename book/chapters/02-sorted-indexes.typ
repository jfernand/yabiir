#import "../template/isss.typ": *

= Sorted Indices <ch-sorted>

#status[Draft. Since yabiir 0.6.0 the keydir can be sorted (`KeyOrder::Sorted`, a `BTreeMap`), and
`Engine::scan` answers a range of keys; since 0.9, `Engine::scan_page` reads it a page at a time in
either direction, so the newest twenty are one call. Composite keys are encoded by `yabiir-sql`, above
the engine, and every output is a real run of this chapter's programs.]

An email program opening an INBOX of 16,720 messages needs the newest twenty. With
a hash keydir that means reading every key in the store — every mailbox of every account — keeping
the ones in that INBOX, and sorting them to keep twenty. A sorted keydir finds them with one descent
of a tree and twenty steps.

== B-trees

A _sorted index_ keeps its keys in order, so that a key's neighbours in the index are its neighbours
in value. Rudolf Bayer and Edward McCreight's B-tree @bayer1972btree is the usual way to keep one: a
shallow tree of wide nodes, each holding a sorted run of keys, rebalanced on insert so every leaf is
at the same depth. Seven years after their paper, Douglas Comer could open a survey with "B-trees
have become, de facto, a standard for file organization" and title it _The Ubiquitous
B-Tree_.#footnote[@comer1979btree. On the name: "The origin of 'B-tree' has never been explained
by the authors. As we shall see, 'balanced,' 'broad,' or 'bushy' might apply. Others suggest that
the 'B' stands for Boeing," where Bayer and McCreight worked. Comer's own suggestion: "it seems
appropriate to think of B-trees as 'Bayer'-trees."] Rust's standard library ships one as
`BTreeMap`.

#figure(
  image("../figures/btree.svg", width: 52%),
  caption: [Chapter 0's five keys in a B-tree with at most two keys per node. A lookup for
  `zx-spectrum` compares it with `c64` at the root, follows the pointer to the right because it sorts
  after, and finds it in that leaf. Read left to right, the leaves hold every key in order. Rust's
  `BTreeMap` holds up to eleven keys per node, so it would keep these five in one.],
)

Looking up one key — the `get` of @ch-keydir, `WHERE key = k` — means comparing it with the keys in
each node on the way down from the root: O(log n) comparisons of whole keys, where the keydir hashes
the key once and goes straight to it. The B-tree is built to do scans, and the hash map is built to
do `get`, so each is faster than the other at the task it was built for.#footnote[With _n_ keys in
the index, and _m_ of them in the answer:
#table(
  columns: (auto, auto, auto),
  [Operation], [Hash map], [B-tree],
  [Look up one key], [O(1)], [O(log n)],
  [Insert or overwrite one key], [O(1)], [O(log n)],
  [Smallest or largest key], [O(n)], [O(log n)],
  [Keys from `a` to `b`], [O(n + m log m)], [O(log n + m)],
  [Every key, in order], [O(n log n)], [O(n)],
)
The hash map's costs are on average, assuming a hash that spreads the keys well. Measured for yabiir
0.6.0 on a million keys, one lookup takes about 25 ns in the hash map and 510 ns in the B-tree.]

Inside a whole store the difference is smaller, because a `get` also reads the value from its file.
We load the same million keys into a store with each keydir, get every key in another order, and
reopen it:#footnote[The program is #repo("book/code/ch02/src/bin/keydir-times.rs"), and every run in
this chapter is in #repo("book/code/ch02/keys.txt"). Each number is the median of three runs, on a
machine that was doing other work too, so only the ratios mean anything.]

#output(title: "$ keydir-times DIR")[```
1000000 keys, median of 3 runs
keydir     put (ns)   get (ns)  open (ms)
hashed         2401        962        478
sorted         3653       1420       1002
```]

The sorted keydir costs about half as much again on every `put` and `get`, and twice as long to
rebuild at open.

== Range scans

Every ordered question is the same two steps: descend to a starting key in O(log n), then walk
forward or backward through the keys in order. In a plain B-tree the walk is the awkward part: the
next key may be several nodes away, back up the path and down another. Comer's survey points to the
variant databases use instead, the _B+-tree_, which keeps every key in the leaves and links the
leaves left to right, so the walk is one leaf after the next @comer1979btree.

#table(
  columns: (auto, 1fr, auto),
  [Operation], [Keys returned], [Example],
  [Range], [From `a` up to `b`.], [Orders in March],
  [Prefix], [Starting with `alice/`.], [Everything one user owns],
  [First or last N], [The N smallest or largest in a range.], [Newest 20 messages],
  [Next or previous], [The one just after or before `k`.], [The next page of results],
  [Nearest], [The largest `≤ k`, or the smallest `≥ k`.], [A record as of time T],
  [Ordered walk], [All of them, in order.], [Merging two sorted streams],
)

"First 1" over a range of pending jobs, ordered by sequence number, is a work queue: an email program
that queues the changes it has to send to the server finds the next one that way. "Nearest", over keys that end in a
timestamp, returns a value as it stood at a given time, without keeping a separate history.

== Composite keys

A _composite key_ is built from several values, compared left to right:

```text
(account, mailbox, uid)

("alice", "INBOX",  41)
("alice", "INBOX",  42)
("alice", "Sent",    7)
("bob",   "INBOX",   3)
```

In a sorted index, every leading part of the tuple selects a contiguous run of keys:

- `("alice")` selects every message Alice has, in every mailbox.
- `("alice", "INBOX")` selects one mailbox.
- `("alice", "INBOX")`, walked backwards with a limit of twenty, selects that mailbox's newest twenty.

Each of these is a prefix scan, and none needs an index of its own.

== Order-preserving encoding

The index compares raw bytes, so each part of a composite key has to be _encoded_ so that comparing
the bytes gives the same answer as comparing the values. The obvious encodings of an integer don't:

#output(title: "$ composite DIR")[```
integers, sorted by their bytes
  as text          -1 100 256 42 7
  little-endian    256 7 42 100 -1
  big-endian       7 42 100 256 -1
  sign bit flipped -1 7 42 100 256
```]

As text, 42 sorts after 100. Little-endian sorts by the lowest byte first. Big-endian gets the
positive numbers right and puts −1 last, because a negative number's top bit is set. Flipping that bit
moves the negatives below the positives and keeps each group in order.

A string needs an end. Without one, the parts run together: `("al", "ice")` and `("alice", "")` are
the same bytes. So a string ends in a `00`, which sorts below every other byte, and that also puts
`"al"` before `"alice"`. A string can contain a `00` of its own, so inside a string it's written
`00 FF`. FoundationDB's tuple layer uses this scheme for strings @fdb-tuples, and ours does
too:#footnote[The module is #repo("crates/yabiir-sql/src/key.rs"). Its tests compare the byte order
with the tuple order for every pair of 507 tuples, chosen to include negative numbers, empty strings
and strings with zeros in them.]

```rust
pub fn encode_key(parts: &[Datum]) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        match part {
            Datum::Integer(n) => {
                out.push(TAG_INTEGER);
                let flipped = (*n as u64) ^ (1 << 63);
                out.extend_from_slice(&flipped.to_be_bytes());
            }
            Datum::Text(s) => {
                out.push(TAG_TEXT);
                for &b in s.as_bytes() {
                    out.push(b);
                    if b == 0 {
                        out.push(0xff);
                    }
                }
                out.push(0);
            }
        }
    }
    out
}
```

Here's the literal key for message 42 of Alice's INBOX:

#output(title: "$ composite DIR")[```
("alice", "INBOX", 42)
02 61 6c 69 63 65 00 02 49 4e 42 4f 58 00 01 80  .alice..INBOX...
00 00 00 00 00 00 2a                             ......*
(23 bytes)
```]

Twenty-three bytes. `02` is the tag for text, then `alice` and its `00`. The second `02` starts
`INBOX`, which ends in `00` too. `01` is the tag for an integer, and `80 00 00 00 00 00 00 2a` is 42
with its sign bit flipped.

A prefix scan needs one more detail. The bytes of `("a")` are `02 61 00`, and the bytes of `("a\0")`,
a different string, are `02 61 00 FF 00`, so "starts with these bytes" would match both. After a
complete part the next byte is a tag, always below `FF`, so the scan runs from the prefix up to, and
not including, the prefix followed by `FF`:

#output(title: "$ composite DIR")[```
scan of the prefix ("alice")
  ("alice", "INBOX", 41)
  ("alice", "INBOX", 42)
  ("alice", "INBOX", 100)
  ("alice", "Sent", 7)
scan of the prefix ("alice", "INBOX")
  ("alice", "INBOX", 41)
  ("alice", "INBOX", 42)
  ("alice", "INBOX", 100)
```]

The store also holds a message of an account named `al`, and neither scan returns it. UID 100 comes
after 42, as a number should.

== Tables as key ranges

A table id in front of every key divides one sorted store into tables, each in its own range:

```text
{table id: u16} {encoded key}

0x0001 ("alice", "INBOX", 41)   → envelope
0x0002 ("alice", "INBOX", 41)   → flags
0x0003 ("alice", "INBOX", 41)   → raw body
```

A scan of table 2 is a prefix scan on `0x0002`, and it can't reach a key of table 1 or 3. That's
a tables layer, and of the storage engine under it, it needs one change: an ordered keydir.

That's a sorted index: slower by a few comparisons per lookup, and able to answer every question in
order. @ch-sql shows how a query in SQL becomes a `get` or a `scan`, and @ch-secondary orders a
table by something other than its key.

#takeaways(
  [A B-tree keeps keys sorted, at the cost of O(log n) comparisons per lookup.],
  [Range, prefix, first/last-N, next/previous and nearest are one descent and a walk.],
  [Composite keys turn every prefix of the tuple into a group, given an order-preserving encoding.],
  [Integers are encoded big-endian with the sign bit flipped; strings end in `00`, and a `00` inside
   one is escaped.],
  [A table is a key prefix, so many tables share one sorted store.],
)

#todo(
  [B-tree against skip list and sorted vector, here or in a later chapter.],
)
// chapter-status:begin
// stage: research-done
// revision: 4
// body: f1c1a395d7bb
// activity: 2026-11-22 | r3 | stage | yabiir | note | migrated from Incipient; prose incomplete
// activity: 2026-11-22 | r3 | stage | yabiir | note | research-done -> early-draft
// activity: 2026-11-23 | r3 | stage | yabiir | note | author: chapters nobody has reviewed are research-complete
// chapter-status:end
