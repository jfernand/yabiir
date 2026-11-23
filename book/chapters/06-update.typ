#import "../template/isss.typ": *

= UPDATE <ch-update>

#status[Draft. `UPDATE` is in `yabiir-sql`, with assignments from a literal or from a field plus an
integer, and every output is a real run of this chapter's programs.]

#epigraph(source: [Justin Sheehy and David Smith, 2010 @sheehy2010bitcask])[The active file is only
written by appending, which means that sequential writes do not require disk seeking.]

Eight threads each add 1 to the same row's year, 2,000 times. That's 16,000 increments. Written the
obvious way, as a read followed by a write, 2,071 of them survive.

A log has no way to change a record in place, so an `UPDATE` reads a row, computes a new one and
appends it. Everything in this chapter is about what can happen between the read and the append.

== UPDATE

`UPDATE` names the table, assigns to one or more fields, and takes the same conditions as `SELECT`
and `DELETE`:

```sql
UPDATE store SET maker = 'cbm' WHERE key = 'c64'
```

#syntax(rr-seq(rr-kw("UPDATE", new: true), rr-kw("store"), rr-kw("SET", new: true),
  rr-loop(rr-seq(rr-ph("field", new: true), rr-kw("=", new: true),
    rr-choice(rr-ph("literal", new: true),
      rr-seq(rr-ph("field", new: true), rr-kw("+", new: true), rr-ph("integer", new: true)))),
    rr-kw(",", new: true)),
  rr-opt(rr-seq(rr-kw("WHERE"), rr-ph("condition")))))

The rows it changes are the rows the same `SELECT *` returns, so the plan is that `SELECT`'s with an
`Update` on top:#footnote[The program is #repo("book/code/ch06/src/main.rs"). Every run in this
chapter is from #repo("book/code/ch06/run.sh"), and its output is in
#repo("book/code/ch06/updates.txt"). The code is `update_rows`, in
#repo("crates/yabiir-sql/src/table.rs").]

#output(title: "$ sql DIR \"UPDATE store SET maker = 'cbm' WHERE key = 'c64'\"", size: 6.8pt)[```
plan:
  Update maker = 'cbm' (one batch)
    Get 'c64' (get: one lookup)
updated 1 row
rows visited: 1
```]

The engine has no operation for changing one field. The whole row is encoded again and appended
under the same key, the keydir moves to the new record, and the old one stays in the file as garbage
until a merge drops it, as a deleted row's does in @ch-delete.

=== Assignments

The right side of an assignment can read the row, and every assignment reads the row as it was before
the statement. A condition on an attribute works as it does in a `SELECT`:

#output(title: "$ sql DIR \"UPDATE store SET year = year + 1 WHERE year = 1981\"", size: 6.8pt)[```
plan:
  Update year = year + 1 (one batch)
    Filter year = 1981
      FullScan (fold: every row)
updated 2 rows
rows visited: 5
```]

The planner checks each assignment against the schema before anything is read:

#output(title: "$ sql DIR \"UPDATE store SET …\"", size: 6.8pt)[```
year = 'soon'
error: year must be an integer; 'soon' isn't

maker = maker + 1
error: maker must be an integer; maker + 1 isn't

year = 1, year = 2
error: year is assigned twice
```]

== Lost updates

#block(sticky: true)[Here's the read and the write of the opening run:#footnote[The full program is
#repo("book/code/ch06/src/bin/update-race.rs").]]

```rust
fn read_then_put(store: &Engine) {
    let schema = computers();
    let bytes = store.get(b"c64").unwrap().unwrap();
    let mut row = schema.decode(&bytes).unwrap();
    let Datum::Integer(year) = row[1] else { unreachable!() };
    row[1] = Datum::Integer(year + 1);
    store.put(b"c64", &schema.encode(&row), yabiir::now_unix()).unwrap();
}
```

Two threads read 1982, both compute 1983, and both write it. Two increments ran and the row moved by
one. This is a _lost update_, and each `put` succeeded, so nothing reports it. `UPDATE` reads each row
with its version #since("0.9") and writes a batch that holds only if that version is still current:

```rust
let Some(current) = store.get_versioned(key)? else { continue };
let old = schema.row(key, &current.value)?;
if !plan.selection.still_selects(&old) {
    continue;
}
let new = plan.apply(&old, schema)?;
batch.push(Op::Check {
    key: key.to_vec(),
    cond: Cond::Version(current.version),
});
batch.push(Op::Put {
    key: key.to_vec(),
    value: schema.encode(&new[1..]),
});
```

If another writer got there first, the check fails, nothing is written, and the statement starts
again from a fresh read:

#output(title: "$ update-race DIR")[```
8 threads, 2000 increments each
                expected   added   lost  retries
read, then put     16000    2071  13929        0
UPDATE             16000   16000      0    14256
```]

Every increment counted, and the price is in the last column: 14,256 attempts ran again. The first version of this code
retried at once, and under this load some threads lost a thousand times in a row and gave up. Each
retry now waits a random time from a range that doubles, up to about a millisecond, so the threads
stop colliding in step.#footnote[After 1,000 attempts the statement reports a conflict and changes
nothing. Random, growing waits are the _exponential backoff_ of Ethernet and of most retry loops.]

A multi-row `UPDATE` puts every row and every check in one batch, so a crash applies all of it or
none, for the reason @ch-delete measured.

== Changing the key

The key is the row's identity, and the store files a record under it, so changing it can't be done in
place. It's a delete of the old key and an insert of the new one. Done as two writes, a crash can
land between them, and which way it goes wrong depends on the order. We change `c64` to `c128`, cut
the log at each byte of the change, and reopen:#footnote[The full program is
#repo("book/code/ch06/src/bin/move-crash.rs").]

#output(title: "$ move-crash DIR")[```
                 what a crash can leave
delete, put      (no row), c128, c64
put, delete      c128, c128 and c64, c64
one batch        c128, c64
```]

Deleting first can lose the row. Putting first can leave it twice, under two keys. One batch leaves
the old row or the new one. `UPDATE` writes that batch with two checks: the old key still has the
version it read, and the new key is absent, because a primary key that's already taken is the same
error as in `INSERT`:

#output(title: "$ sql DIR \"UPDATE store SET key = 'ibm-pc' WHERE key = 'c128'\"", size: 6.8pt)[```
plan:
  Update key = 'ibm-pc' (one batch)
    Get 'c128' (get: one lookup)
error: key 'ibm-pc' already exists
```]

Assigning one literal key to several rows fails the same way, since they can't all have it.

That's `UPDATE`: a read, a check that the read still holds, and an append.

#takeaways(
  [An `UPDATE` appends a whole new record; the old one is garbage until a merge.],
  [Every assignment is computed from the row as it was before the statement.],
  [A read followed by a `put` loses updates, silently.],
  [A batch with `Cond::Version` makes the read and the write one decision, and a failed check starts
   the statement over.],
  [Retries need a random, growing wait, or the same writers keep colliding.],
  [Changing a key is a delete and an insert, in one batch, with a check that the new key is free.],
)

#todo(
  [Expressions beyond `field + integer`.],
)
// chapter-status:begin
// stage: research-done
// revision: 2
// body: d19b52c2c79b
// activity: 2026-11-22 | r1 | stage | yabiir | note | new chapter, not reviewed by the author
// activity: 2026-11-23 | r1 | stage | yabiir | note | author: chapters nobody has reviewed are research-complete
// chapter-status:end
