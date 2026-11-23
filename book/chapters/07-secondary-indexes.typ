#import "../template/isss.typ": *

= Secondary Indices <ch-secondary>

#status[Draft. The index layer is in `yabiir-sql`, on yabiir 0.9's atomic write batch, and every output
is a real run of this chapter's programs. The SQL planner doesn't use it yet.]

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

== An index layer

yabiir-sql's index layer stores a table and its indices in one sorted store, using the composite keys
of @ch-sorted. The first part of each key says which range it belongs to:#footnote[The code is
#repo("crates/yabiir-sql/src/index.rs"), and every run in this chapter is from
#repo("book/code/ch07/run.sh"), with its output in #repo("book/code/ch07/indexes.txt").]

```text
(1, key)                → the row's attributes
(2 + i, value, key)     → nothing: index i says this row has this value
```

Maintenance lives in this layer, so a program can't write a row and forget its index. An insert is
one batch: a check that the key is absent, the row, and one entry per index. A change reads the row
with its version and swaps the entries whose value moved:

```rust
let mut batch = vec![
    Op::Check {
        key: row_key.clone(),
        cond: Cond::Version(current.version),
    },
    Op::Put {
        key: row_key.clone(),
        value: self.schema.encode(&new[1..]),
    },
];
for (before, after) in self.entries(&old).into_iter().zip(self.entries(&new)) {
    if before != after {
        batch.push(Op::Delete { key: before });
        batch.push(Op::Put { key: after, value: Vec::new() });
    }
}
```

A lookup scans the entries for the value, then gets each row. It tests the row's value again, because
the row can change between the two steps.

=== What it costs and what it buys

We load 200,000 rows with zero to three indices, then ask for one year with and without
one:#footnote[The program is #repo("book/code/ch07/src/bin/index-times.rs"). Each time is the median
of five runs, on a machine that was doing other work too, so only the rough ratios mean anything.]

#output(title: "$ index-times DIR")[```
200000 rows; each time is the median of 5 runs
indices   insert (ns)    bytes/row
0                4197           83
1                6836          122
2                7374          160
3               10479          199

WHERE year = 1982: 6667 rows
  full scan    137.68ms
  index         13.05ms
```]

Each index adds about 39 bytes to every row on disk, and three of them make an insert about two and
a half times slower. In exchange, a query that returns one row in thirty runs about ten times faster.
The gap grows with the table, since the scan reads every row and the index reads only the matches.

=== Crashes and races

Written as one `put` per key, a crash can leave a row without its entries. We insert one row with two
indices, cut the log at every byte of the insert, reopen, and compare every row with every
entry:#footnote[The programs are #repo("book/code/ch07/src/bin/index-crash.rs") and
#repo("book/code/ch07/src/bin/index-race.rs").]

#output(title: "$ index-crash DIR")[```
                  cuts disagreements
one put per key    191           112
one batch          143             0
```]

A batch alone doesn't protect an update. Two threads changing the same row's year can both read the
old value, both delete its entry, and each add their own. One row, two entries, and a query on the
loser's year finds a row that doesn't have it. We run that 5,000 times:

#output(title: "$ index-race DIR")[```
                 rounds stale index rows
no version check   5000             4958
version check      5000                0
```]

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
two prefixes at once. Here's one built over the 47 source files of yabiir 0.9.1, with one key per word
and file:#footnote[The program is #repo("book/code/ch07/src/bin/inverted.rs"). A word is a run of
at least three ASCII letters or digits, lower-cased.]

#output(title: "$ inverted DIR crates/yabiir/src tombstone version")[```
47 files, 1736 words, 8360 postings
tombstone: 17 files
version: 21 files
tombstone AND version: 11 files, in 27 steps
```]

The walk takes 27 steps through two lists of 17 and 21, because both are in order and it never has
to go back.

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
  [Measured: each index adds a write to every insert, and turns a scan into a read of the matches.],
)

#todo(
  [The planner choosing an index by itself, instead of a program calling it.],
)
// chapter-status:begin
// stage: research-done
// revision: 6
// body: e2eebc4841ce
// activity: 2026-11-22 | r5 | stage | yabiir | note | migrated from Incipient; prose incomplete
// activity: 2026-11-22 | r5 | stage | yabiir | note | research-done -> early-draft
// activity: 2026-11-23 | r5 | stage | yabiir | note | author: chapters nobody has reviewed are research-complete
// chapter-status:end
