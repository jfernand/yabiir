#import "../template/isss.typ": *

= Sorted Indices <ch-sorted>

#status[Incipient. The operations and composite keys are described; the order-preserving encoding
needs a worked section. Since yabiir 0.6.0 the keydir can be sorted (`KeyOrder::Sorted`, a `BTreeMap`),
and `Engine::scan` answers a range of keys; composite keys and their encoding are not implemented.]

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
a tables layer, and of the storage engine under it, it needs one change: an ordered keydir.

That's a sorted index: slower by a few comparisons per lookup, and able to answer every question in
order. @ch-sql shows how a query in SQL becomes a `get` or a `scan`, and @ch-secondary orders a
table by something other than its key.

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
