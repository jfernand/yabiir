#import "../template/isss.typ": *

= DELETE <ch-delete>

#status[Incipient. `DELETE` by key and by condition is in `yabiir-sql`, and every output is a real run
of this chapter's programs.]

#epigraph(source: [Justin Sheehy and David Smith, 2010 @sheehy2010bitcask])[Note that deletion is simply
a write of a special tombstone value, which will be removed on the next merge.]

We fill a store with 100,000 rows and delete half of them. Before the `DELETE` its data files hold
6,500,024 bytes. After it, they hold 7,400,048.#footnote[The program is
#repo("book/code/ch05/src/bin/space.rs"). Every run in this chapter is from
#repo("book/code/ch05/run.sh"), and its output is in #repo("book/code/ch05/deletes.txt").]

A log can't take anything back, so removing a row is one more thing to append. Three questions follow
from that: what gets written, what a crash halfway through a many-row `DELETE` leaves behind, and
when the space comes back.

== DELETE by key

`DELETE` names a table, as `INSERT` does, and takes the same conditions as `SELECT`:#footnote[`SELECT`
still has no `FROM`, for the reason @ch-attributes gave. In `DELETE` the word is part of SQL's grammar
@iso9075, and any table but `store` is an error.]

```sql
DELETE FROM store WHERE key = 'c64'
```

#syntax(rr-seq(rr-kw("DELETE", new: true), rr-kw("FROM", new: true), rr-kw("store"),
  rr-opt(rr-seq(rr-kw("WHERE"), rr-ph("field"),
    rr-choice(
      rr-seq(rr-kw("="), rr-ph("literal")),
      rr-seq(rr-kw("BETWEEN"), rr-ph("literal"), rr-kw("AND"), rr-ph("literal")))))))

The rows a `DELETE` removes are the rows a `SELECT *` with the same condition returns, so the planner
builds that plan and puts a `Delete` on top. A condition on the key is one lookup:

#output(title: "$ sql DIR \"DELETE FROM store WHERE key = 'c64'\"")[```
plan:
  Delete (one batch)
    Get 'c64' (get: one lookup)
deleted 1 row
rows visited: 1
```]

=== The tombstone

The row's record is still in the data file, untouched. What the `DELETE` added is a _tombstone_: a
record that says this key no longer has a value. Here's the literal end of the file:

#output(title: "$ dump-tail DIR 35")[```
c6 8d a8 e8 0b 00 00 00 06 00 00 00 00 00 00 00  ................
aa b5 03 6b 01 00 00 00 03 00 00 80 00 00 00 00  ...k............
63 36 34                                         c64
(35 bytes)
```]

Thirty-five bytes. `c6 8d a8 e8` is the CRC, and `0b 00 00 00` says 11 bytes follow this record's
24-byte header. The next eight bytes are the sequence number, 6: this is the sixth write to the store.
`aa b5 03 6b` is the timestamp and `01 00 00 00` the number of operations. The operation is the last
eleven bytes: `03 00 00 80` is the key's length, 3, with the top bit set, and that bit is the whole
difference between a tombstone and a put. `00 00 00 00` is the length of a value it doesn't have, and
`63 36 34` spells `c64`.

Each run of `sql` is a new process, and opening the store rebuilds the keydir by reading the log from
the start. It meets the row's record, adds the key, then meets the tombstone and removes it:

#output(title: "$ sql DIR \"SELECT * WHERE key = 'c64'\"")[```
plan:
  Get 'c64' (get: one lookup)
 key | maker | year
-----+-------+------
(0 rows)
rows visited: 0
```]

Without the tombstone, the deleted row would come back at the next open. Deleting a key that isn't
there writes nothing, since there's nothing for a tombstone to hide, and reports `deleted 0 rows`.

== DELETE by condition

A condition on an attribute is the `Filter` over a `FullScan` of @ch-attributes, and it's the first
write in this book that applies to a set of rows:

#output(title: "$ sql DIR \"DELETE FROM store WHERE year = 1982\"")[```
plan:
  Delete (one batch)
    Filter year = 1982
      FullScan (fold: every row)
deleted 2 rows
rows visited: 5
```]

Conditions are checked against the schema first, as in a `SELECT`, so `WHERE year = '1981'` is a
domain error and deletes nothing.

#warn[With no `WHERE`, the condition is true of every row. `DELETE FROM store` empties the table.]

=== Atomicity

The direct way to run that plan is one `delete` per match. Each is a record of its own, so a crash
between two of them leaves the statement half done. To see every state a crash can leave, we remove
four of five rows, then cut the log at each byte the removal wrote and reopen the store from what's
left:#footnote[The full program is #repo("book/code/ch05/src/bin/delete-crash.rs"). A cut models a
write that stopped partway. It doesn't model a disk that reorders writes.]

#output(title: "$ delete-crash DIR")[```
               bytes  cuts   rows after reopening
one-at-a-time    157   158   5, 4, 3, 2, 1
one-batch         85    86   5, 1
```]

`bytes` is what the removal appended, `cuts` is how many lengths of the file we tried, and the last
column lists every row count that came back. One delete at a time, a crash can stop at any row. The
batch of @sec-batches is one record under one CRC, so a cut anywhere inside it leaves a record cut short, and
recovery drops the whole record: the store has all five rows or the one that was meant to stay. It's
also smaller, because the four operations share a 24-byte header.#footnote[A batch's operations have
to fit in a body of $2^32 - 1$ bytes, so a `DELETE` of a few hundred million rows fails with `BatchTooLarge`
and removes nothing.]

=== Concurrent writes

Atomicity leaves one problem. Between the scan that finds a row and the tombstone that removes it,
another writer can change the row:

```rust
fn scan_then_delete(store: &Engine) {
    let matches = store.fold(/* the keys whose year is 1982 */);
    for key in matches {
        store.delete(&key, yabiir::now_unix()).unwrap();
    }
}
```

To measure it, one thread runs `DELETE FROM store WHERE year = 1982` while another moves the only row
to 1990. The row should survive in either order: if the `DELETE` goes first, the move writes the row
again, and if the move goes first, the row no longer matches. We run that race 20,000
times and count the rounds that end with no row:#footnote[The full program is
#repo("book/code/ch05/src/bin/delete-race.rs"). The machine was doing other work too, so the count
will differ from run to run.]

#output(title: "$ delete-race DIR")[```
                   rounds  rows lost
scan, then delete   20000       2126
guarded batch       20000          0
```]

In 2,126 rounds the `DELETE` removed a row from 1990, which its condition excludes. The guarded
version reads each match again with its version #since("0.9"), tests the condition on what it read,
and puts a check on that version in the batch beside the delete:

```rust
for row in &found.rows {
    let Datum::Text(key) = &row[0] else { unreachable!() };
    let key = key.as_bytes();
    let Some(current) = store.get_versioned(key)? else {
        continue;
    };
    if plan.still_selects(&schema.row(key, &current.value)?) {
        batch.push(Op::Check {
            key: key.to_vec(),
            cond: Cond::Version(current.version),
        });
        batch.push(Op::Delete { key: key.to_vec() });
    }
}
```

If any of those rows changes before the batch commits, its check fails, nothing is written, and the
statement starts again from a new scan.#footnote[It waits a random, growing time between attempts, and after 1,000 it reports a conflict and
removes nothing. The code is `delete_rows`, in #repo("crates/yabiir-sql/src/table.rs").] The guard
covers the rows the scan found. A row that starts to match after the scan isn't one of them, and it
stays.

== Merge

So far a `DELETE` has only added bytes. The space comes back in _merge_, which Sheehy and Smith's
design runs over the files the store has finished writing @sheehy2010bitcask. It copies each key's
live record to new files, then removes the old ones. Replaced values, deleted rows and their tombstones
are simply not copied. A tombstone can go at that point because the older records it was hiding go
with it.

Merge also writes a _hint file_ beside each new data file: one small entry per key, saying where its
value is, so that the next open can build the keydir without reading the values. Here's the store from
the start of the chapter at each stage:

#table(
  columns: (1fr, auto, auto, auto),
  [Stage], [Rows], [Data bytes], [Hint bytes],
  [Inserted], [100,000], [6,500,024], [0],
  [After `DELETE` of half], [50,000], [7,400,048], [0],
  [After merge], [50,000], [3,250,024], [1,900,012],
)

Each row takes 65 bytes, and each tombstone in the batch takes 18: an 8-byte operation header and a
10-byte key. Those 50,000 tombstones are the 900,024 bytes the `DELETE` added, counting the record's
header. Merge halves the data, to exactly 65 bytes for each of the 50,000 rows left, plus the file's
header. The hint file costs 38 bytes per row, so the store as a whole goes from 7.4 MB to 5.15 MB.

That's `DELETE` on a log: a few bytes that say a row is gone, and a later pass that makes it true.
Changing a row instead of removing it writes a new record the same way, and that's @ch-update.

#takeaways(
  [A `DELETE` appends a _tombstone_; the row's record stays in the file until a merge.],
  [Opening the store replays tombstones, so a deleted row stays deleted across a restart.],
  [The rows a `DELETE` removes are the rows the same `SELECT *` returns, and it uses the same plan.],
  [One `delete` per row can stop halfway in a crash; one batch is applied whole or not at all.],
  [A row can change between the scan and the tombstone; a version check in the batch catches it.],
  [Disk space comes back at merge, which copies live records and drops the rest.],
)

#todo(
  [When to merge: the cost of running it against the space it recovers.],
  [Conditions beyond `=` and `BETWEEN`.],
)
// chapter-status:begin
// stage: research-done
// revision: 2
// body: 9a98b9f5f3dc
// activity: 2026-11-22 | r1 | stage | yabiir | note | migrated from Incipient; text complete, not reviewed by the author
// activity: 2026-11-23 | r1 | stage | yabiir | note | author: chapters nobody has reviewed are research-complete
// chapter-status:end
