#import "../template/isss.typ": *

= Sorted Indexes <ch-sorted>

#status[Incipient. The operations and composite keys are described; the order-preserving encoding
needs a worked section, and nothing here is implemented in yabiir yet.]

The email-mcp cache needs the newest twenty messages in an iCloud INBOX of 16,720 @isss-tr-0503. With
a hash keydir that means reading every key in the store — every mailbox of every account — keeping
the ones in that INBOX, and sorting them to keep twenty. A sorted keydir finds them with one descent
of a tree and twenty steps.

== B-trees

A _sorted index_ keeps its keys in order, so that a key's neighbours in the index are its neighbours
in value. Rudolf Bayer and Edward McCreight's B-tree @bayer1972btree is the usual way to keep one: a
shallow tree of wide nodes, each holding a sorted run of keys, rebalanced on insert so every leaf is
at the same depth. Rust's standard library ships one as `BTreeMap`.

A point lookup in a B-tree compares the key against the keys in each node on the way down — O(log n)
comparisons of whole keys, against one hash for the keydir of @ch-keydir. The B-tree is slower for
exactly the question the hash map is built for, by an amount yabiir's benchmarks will have to
measure.

== Range scans

Every ordered question is the same two steps: descend to a starting key in O(log n), then walk
forward or backward through the keys in order.

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

"First 1" over a range of pending jobs, ordered by sequence number, is a work queue: the email-mcp
command queue finds its next command that way @isss-tr-0503. "Nearest", over keys that end in a
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

#warn(title: "Byte order must equal logical order")[
  The index compares raw bytes. Written as text, 42 sorts after 100; written little-endian, it sorts
  by its lowest byte. Each part of a composite key has to be _encoded_ so that comparing the bytes
  gives the same answer as comparing the values: big-endian integers, the sign bit flipped for
  signed ones, and strings with a terminator that sorts below every character, so that `"al"` comes
  before `"alice"`. FoundationDB's tuple layer is the standard reference @fdb-tuples.
]

== Tables as key ranges

A table id in front of every key divides one sorted store into tables, each in its own range:

```text
{table id: u16} {encoded key}

0x0001 ("alice", "INBOX", 41)   → envelope
0x0002 ("alice", "INBOX", 41)   → flags
0x0003 ("alice", "INBOX", 41)   → raw body
```

A scan of table 2 is a prefix scan on `0x0002`, and it can't reach a key of table 1 or 3. That's
the `yabiir-tables` layer planned for email-mcp @isss-tr-0503, and of the storage engine under it, it
needs one change: an ordered keydir.

That's a sorted index: slower by a few comparisons per lookup, and able to answer every question in
order. @ch-secondary orders a table by something other than its key.

#takeaways(
  [A B-tree keeps keys sorted, at the cost of O(log n) comparisons per lookup.],
  [Range, prefix, first/last-N, next/previous and nearest are one descent and a walk.],
  [Composite keys turn every prefix of the tuple into a group, given an order-preserving encoding.],
  [A table is a key prefix, so many tables share one sorted store.],
)

#todo(
  [A worked section on the order-preserving encoding, string escaping included.],
  [Measured: hash keydir against `BTreeMap` for `get`, `insert` and the rebuild at open.],
  [B-tree against skip list and sorted vector, here or in a later chapter.],
)
