#import "../template/isss.typ": *

= Secondary Indices <ch-secondary>

#status[Incipient. The idea and its costs are stated; nothing is designed in detail, and the
consistency story is open.]

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
  [Consistency], [The record and its index entries are separate writes. A crash between them leaves
    them disagreeing, until a transaction, a write order or a rebuild puts them back.],
)

#note(title: "Rebuildable or not")[
  A cache can take the simplest answer to consistency: discard the index and rebuild it from the
  table. A store that holds the only copy of its data can't, and that's where transactions start to
  pay for their complexity.
]

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
  [An inverted index is a secondary index with an entry per word; queries intersect its postings.],
)

#todo(
  [Where index maintenance lives: declared in the table layer, or written by the application.],
  [One consistency story — write order plus rebuild, or a minimal transaction — chosen and built.],
  [An inverted index built for real over the Bluesky firehose posts that `gulp` stores.],
)
