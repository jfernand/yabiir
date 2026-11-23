#import "../template/isss.typ": *

= Secondary Indices <ch-secondary>

#status[Incipient. The idea and its costs are stated. yabiir 0.9 has the atomic write batch that keeps
a record and its index entries consistent; the index layer that uses it isn't built.]

A mailbox stored by `(account, mailbox, uid)` lists its messages in UID order, and an email
program's search asks for them by sender and by date. Without another index it answers those by
scanning the whole mailbox: every record of a 16,720-message INBOX, to return a handful. An index on the sender or the
date replaces that scan with a range scan.

== Secondary indices

A _secondary index_ maps the value of a field to the primary keys of the records that have it. A
lookup through it takes two steps: a scan of the index for primary keys, then a `get` of each record.

In a sorted store, a secondary index is one more table. Its key is the field's value followed by the
record's primary key, and its value is empty:

```text
by_date:    (acct, mbox, date, uid)          → ()
by_sender:  (acct, sender_email, mbox, uid)  → ()
```

The primary key at the end makes every index key unique, even when two messages share a date. Every
operation of @ch-sorted then applies to the field: a range of `by_date` returns the messages between
two dates, its last twenty the twenty newest by date, and a prefix of `by_sender` everything from one
sender.

A _unique_ index allows each value once. A write first looks the value up and refuses it if another
record already has it — "no two accounts share an email address."

A _covering_ index stores a few of the record's fields in its value as well, so a query that needs
only those fields is answered from the index without a second lookup.

== Index maintenance

Every index makes reads cheaper and writes more expensive:

#table(
  columns: (auto, 1fr),
  [Cost], [Cause],
  [Write amplification], [A `put` writes the record and one entry per index: five indices, six
    writes.],
  [Read before write], [Changing an indexed field means deleting the old index entry, so the old
    record has to be read first to know what it was.],
  [Consistency], [The record and its index entries are separate keys. Written one `put` at a time,
    a crash between them leaves them disagreeing; written as one batch, they can't.],
)

#note(title: "Rebuildable or not")[
  A cache can take the simplest answer to consistency: discard the index and rebuild it from the
  table. A store that holds the only copy of its data can't, and that's where transactions start to
  pay for their complexity.
]

== Atomic write batches <sec-batches>

A _write batch_ is a list of writes that the store applies together or not at all. yabiir's has a
third kind of operation beside the put and the delete, a _check_, which writes nothing and fails the
whole batch unless its key meets a condition #since("0.9"):

#srcfile("batch.rs", tag: "v0.9.0", root: "crates/yabiir/src/", lines: ((19, 25), (30, 37)))[```rust
pub enum Op {
    /// Store `value` under `key`.
    Put { key: Vec<u8>, value: Vec<u8> },
    /// Remove `key`. Removing a key that isn't there does nothing.
    Delete { key: Vec<u8> },
    /// Write nothing, but fail the whole batch unless `key` meets `cond`.
    Check { key: Vec<u8>, cond: Cond },
// …
pub enum Cond {
    /// The key has no value.
    Absent,
    /// The key has a value.
    Exists,
    /// The key has a value, and its version is exactly this one (see
    /// [`Engine::get_versioned`](crate::Engine::get_versioned)).
    Version(u64),
```]

`Engine::write_batch` takes a `Vec<Op>` and returns a `Commit`, which says how the batch ended. Every
check is evaluated first, against the store as it was just before the batch; if one fails, nothing is
written and the commit reports the check's position. On disk the batch is one record under one CRC,
so recovery replays all of it or none of it: a crash in the middle of the append leaves a record cut
short, which recovery drops like any torn write.#footnote[yabiir's tests cut the file at every byte
of a batch's record and reopen it: the store holds the whole batch or nothing of it. The test is
`a_batch_cut_anywhere_by_a_crash_recovers_whole_or_not_at_all`, in
#repo("crates/yabiir/src/engine/tests/batches.rs").]

Two of the three costs change with a batch; the writes are as many as before. A new message goes in as
one batch: the record and an entry in each index, so no crash leaves an index entry without its
record. The read before a write gets a guard: every write gives the keys it puts a new _version_, the write's sequence
number in the log, which `Engine::get_versioned` returns with the value. A change of date reads the
record and its version, then writes one batch: a check that the record's version is still the one it
read, the new record, a delete of the old `by_date` entry and a put of the new one. If another writer
changed the record in between, the check fails, nothing is written, and the change starts again from
a fresh read.

A version is never handed out twice, not even to a key that was deleted and written again, so the
check can't mistake a recreated record for the one that was read.#footnote[A version that restarted
at 1 on every recreate would let a stale check pass: read version 1, someone deletes and recreates the
record, and `Version(1)` matches the new one. yabiir's versions come from one counter for the whole
store, and survive merge and reopen: merge writes the counter's value into every file it creates,
so the versions of the deleted keys it drops are still accounted for.]

== Inverted indices

An _inverted index_ is the secondary index of a search engine, with many entries per record. Each
document is split into words, and the index maps every word to the documents that contain it, its
_postings_:

```text
postings:  (word, doc_id) → ()

("bitcask", 12) ("bitcask", 40)
("rust",     7) ("rust",    12) ("rust", 40)
```

Its operations work on postings rather than single keys. `rust AND bitcask` _intersects_ two
postings — documents 12 and 40 — `OR` merges them, and a ranking orders the result. Each word's
postings are one prefix of the index, in document order, so the intersection is an ordered walk over
two prefixes at once.

== Other index structures

A single sort order can't do the work of these:

- _Aggregate trees_ store a count or a sum in each tree node, so the number of keys in a range is
  read from O(log n) nodes instead of counted.
- _Multi-dimensional indices_, such as Antonin Guttman's R-tree @guttman1984rtree, answer "points
  inside this rectangle," which no ordering of points by one coordinate answers well.
- _Bloom filters_ @bloom1970 spend a few bits per key to answer "definitely not here," and storage
  engines use them to skip files that can't contain a key.

That's a secondary index: one more table, paid for on every write.

#takeaways(
  [A secondary index maps a field's value to primary keys; in a sorted store it's one more table.],
  [It gives that field every ordered operation: ranges, prefixes, newest N.],
  [Each index costs a write per `put`, a read before each update, and a consistency story.],
  [An atomic batch with a version check keeps a record and its index entries consistent, through
   crashes and concurrent writers.],
  [An inverted index is a secondary index with an entry per word; queries intersect its postings.],
)

#todo(
  [Where index maintenance lives: declared in the table layer, or written by the application.],
  [An index layer on `write_batch`: the record and its entries in one batch, updates guarded by
   `Cond::Version`, measured against one `put` per key.],
  [An inverted index built for real over the Bluesky firehose posts that `gulp` stores.],
)
// chapter-status:begin
// stage: research-done
// revision: 5
// body: 09756ec2c862
// activity: 2026-11-22 | r5 | stage | yabiir | note | migrated from Incipient; prose incomplete
// chapter-status:end
