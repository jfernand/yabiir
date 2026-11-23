#import "../template/isss.typ": *

= Attributes and INSERT <ch-attributes>

#status[Incipient. Domains, the schema, the row encoding, `INSERT` and conditions on attributes are in
place, in `yabiir-sql`, and every output is a real run of this chapter's programs. The schema is
still given by the program; nothing stores it.]

#epigraph(source: [E. F. Codd, 1970 @codd1970])[We shall refer to $S_j$ as the $j$th domain of $R$.]

Our table can say that the ZX Spectrum went on sale in 1982 and nothing else about it. To record who
made it, the value has to hold two facts, and the store has no opinion on what a value holds: to
yabiir it's a run of bytes. Everything in this chapter happens above the engine, which doesn't change.
The table gets a third field:

#table(
  columns: (auto, auto, auto),
  [key], [maker], [year],
  [`apple-ii`], [`apple`], [`1977`],
  [`ibm-pc`], [`ibm`], [`1981`],
  [`bbc-micro`], [`acorn`], [`1981`],
  [`zx-spectrum`], [`sinclair`], [`1982`],
  [`c64`], [`commodore`], [`1982`],
)

Until now the rows came from yabiir's `kv` tool, which writes whatever bytes it's handed. A row with
fields has to be written by something that knows the fields, so the first statement of this book
that isn't a `SELECT` arrives here, with the structure it needs.

== Domains

Codd defines a relation over a list of sets: "$R$ is a relation on these $n$ sets if it is a set of
$n$-tuples each of which has its first element from $S_1$, its second element from $S_2$, and so on."
Each set is a _domain_ of the relation @codd1970. A domain says what an attribute can hold, and it's
what makes `1982` a year and `'sinclair'` not one.

yabiir's table layer has two domains, text and integers (64 bits, signed):#footnote[All the code in
this chapter that isn't a program of its own is in #repo("crates/yabiir-sql/src/table.rs").]

```rust
pub enum Domain {
    Text,
    Integer,
}

pub enum Datum {
    Integer(i64),
    Text(String),
}
```

A `Datum` is one attribute's value in one row. A _schema_ is the list of attributes, each with its
domain. The key is always the first field and always text; the schema lists what goes in the value:

```rust
Schema::new(&[("maker", Domain::Text), ("year", Domain::Integer)])
```

The program hands the schema to the layer each time it runs. Nothing in the store records it, so a
program with the wrong schema is reading someone else's bytes; the encoding below catches most of
those mistakes, and a catalog would catch them all. One table doesn't need a catalog yet.

== The row encoding

A value holds its attributes one after another, in schema order. Each starts with a one-byte _tag_
for its domain. An integer is then 8 bytes, little-endian; text is a 4-byte little-endian length and
its UTF-8:

```rust
pub fn encode(&self, attributes: &[Datum]) -> Vec<u8> {
    let mut out = Vec::new();
    for datum in attributes {
        match datum {
            Datum::Integer(n) => {
                out.push(TAG_INTEGER);
                out.extend_from_slice(&n.to_le_bytes());
            }
            Datum::Text(s) => {
                out.push(TAG_TEXT);
                out.extend_from_slice(&(s.len() as u32).to_le_bytes());
                out.extend_from_slice(s.as_bytes());
            }
        }
    }
    out
}
```

Here's the literal value stored under `zx-spectrum`:#footnote[Every run in this chapter is from
#repo("book/code/ch04/run.sh"); its output is in #repo("book/code/ch04/attributes.txt"). The value
is printed by #repo("book/code/ch04/src/bin/dump-value.rs").]

#output(title: "$ dump-value DIR zx-spectrum")[```
02 08 00 00 00 73 69 6e 63 6c 61 69 72 01 be 07  .....sinclair...
00 00 00 00 00 00                                ......
(22 bytes)
```]

Twenty-two bytes. `02` is the tag for text, `08 00 00 00` its length, and the next eight bytes spell
`sinclair`. `01` is the tag for an integer, and `be 07 00 00 00 00 00 00` is `0x07be`, 1982.

Strictly, the schema makes the tags redundant: it already says the first field is text. They're there
for the day the schema is wrong. Decoding requires one datum per attribute, each with the tag its
domain expects, and no bytes left over, so a value written under another schema, or one of Chapter
0's bare `1982`s, is an error and never a row with a nonsense year.

== INSERT

`INSERT` adds one row. SQL requires the name of a table after `INTO`, and ours is the one the algebra
has been calling `store`:#footnote[`SELECT` still has no `FROM`: there's one table, and a query can't
mean any other. `INTO store` is there because SQL's grammar has no form without it @iso9075. Any
other name is an error.]

```sql
INSERT INTO store VALUES ('zx-spectrum', 'sinclair', 1982)
```

#syntax(rr-seq(rr-kw("INSERT", new: true), rr-kw("INTO", new: true), rr-kw("store", new: true),
  rr-kw("VALUES", new: true), rr-kw("(", new: true),
  rr-loop(rr-ph("literal", new: true), rr-kw(",", new: true)), rr-kw(")", new: true)))

A _literal_ is a string in single quotes or an integer. The values go in the order of the table's
fields, the key first.

=== The primary key

`put` and `INSERT` are different operations. `put` stores a value under a key and replaces whatever
was there. `INSERT` adds a tuple to a relation, and a relation is a set in which the key identifies
each tuple: Codd calls a domain whose values "uniquely identify each element" a _primary key_
@codd1970. Inserting a key the table already holds has to fail.

The obvious translation looks the key up first:

```rust
fn get_then_put(store: &Engine, key: &[u8], value: &[u8]) -> bool {
    if store.get(key).unwrap().is_some() {
        return false;
    }
    store.put(key, value, yabiir::now_unix()).unwrap();
    true
}
```

With one writer it's correct. With two, both can run the `get` before either runs the `put`, both see
no row, and both report success; the second `put` silently replaces the first row. To see how often,
eight threads insert the same key at the same moment, for a thousand keys:#footnote[The full program
is #repo("book/code/ch04/src/bin/insert-race.rs"). A barrier lines the threads up before each key.]

#output(title: "$ insert-race DIR")[```
1000 keys, 8 threads
                 ok   keys twice+  rows lost
get then put   4631           887       3631
batch          1000             0          0
```]

887 keys out of 1,000 were "inserted" by more than one thread (`keys twice+`), and 3,631 callers
were told their row was stored when it had been overwritten (`rows lost`). The `batch` row is the
fix. A write batch #since("0.9") holds a _check_ and a put, and the writer thread evaluates the check and applies the put as one step, so no
other write can land between them (@sec-batches has the rest of the API):

```rust
let batch = vec![
    Op::Check {
        key: key.clone(),
        cond: Cond::Absent,
    },
    Op::Put {
        key: key.clone(),
        value,
    },
];
match store
    .write_batch(batch)
    .wait()
{
    Ok(_) => Ok(()),
    Err(yabiir::Error::ConditionFailed { .. }) => Err(Error::DuplicateKey(
        String::from_utf8_lossy(&key).into_owned(),
    )),
    Err(e) => Err(e.into()),
}
```

A second `INSERT` of the same key now fails, and the row that was there stays:

#grid(columns: (1fr,), row-gutter: 4pt,
  output(title: "$ sql DIR \"INSERT INTO store VALUES ('c64', 'atari', 1999)\"", size: 6.5pt)[```
error: key 'c64' already exists
```],
  output(title: "$ sql DIR \"SELECT * WHERE key = 'c64'\"")[```
plan:
  Get 'c64' (get: one lookup)
 key | maker     | year
-----+-----------+------
 c64 | commodore | 1982
(1 row)
rows visited: 1
```],
)

=== Values outside their domains

`INSERT` checks each value against its field's domain before it encodes anything, so a row that
doesn't fit never reaches the store:

#output(title: "$ sql DIR \"INSERT INTO store VALUES …\"", size: 6.5pt)[```
('amiga', 'commodore', 'nineteen eighty-five')
error: year must be an integer; 'nineteen eighty-five' isn't

('amiga', 'commodore')
error: store has 3 fields (key, maker, year); got 2 values
```]

There's no way to leave a field out. SQL would fill it with `NULL`; this book's relations have no
`NULL`, for the reasons in @app-bags, so a row has a value for every attribute. Codd's own rule for
the key, _entity integrity_, is the narrow form of that position: no primary key value may be null
@codd1979.

== Projection on attributes

The fields after `SELECT` are now a list, and any field of the table can be in it:

#syntax(rr-seq(rr-kw("SELECT"), rr-opt(rr-kw("DISTINCT")),
  rr-choice(rr-kw("*"), rr-loop(rr-ph("field", new: true), rr-kw(",", new: true))),
  rr-opt(rr-seq(rr-kw("WHERE"), rr-ph("field", new: true),
    rr-choice(
      rr-seq(rr-kw("="), rr-ph("literal", new: true)),
      rr-seq(rr-kw("BETWEEN"), rr-ph("literal", new: true), rr-kw("AND"),
        rr-ph("literal", new: true)))))))

This is the projection @ch-select defined, with something to project. The plan has the shape of
@ch-sql's; the leaf produces whole rows, decoded, and `Project` keeps the fields it was asked
for:#footnote[The program is #repo("book/code/ch04/src/main.rs"). `rows visited` counts the records
the plan's leaf read from the store.]

#output(title: "$ sql DIR \"SELECT key, year\"")[```
plan:
  Project key, year
    FullScan (fold: every row)
 key         | year
-------------+------
 bbc-micro   | 1981
 apple-ii    | 1977
 ibm-pc      | 1981
 zx-spectrum | 1982
 c64         | 1982
(5 rows)
rows visited: 5
```]

The planner's rule for `DISTINCT` carries over from @ch-sql unchanged: a result that keeps the key
can't repeat, so `Distinct` goes on only when the key has been projected away. `SELECT DISTINCT year`
gets one; `SELECT DISTINCT key, year` doesn't:

#grid(columns: (1fr, 1fr), column-gutter: 10pt,
  output(title: "$ sql DIR \"SELECT DISTINCT year\"")[```
plan:
  Distinct
    Project year
      FullScan (fold: every row)
 year
------
 1981
 1982
 1977
(3 rows)
rows visited: 5
```],
  output(title: "$ sql DIR \"SELECT DISTINCT key, year\"", size: 6.6pt)[```
plan:
  Project key, year
    FullScan (fold: every row)
 key         | year
-------------+------
 apple-ii    | 1977
 bbc-micro   | 1981
 c64         | 1982
 ibm-pc      | 1981
 zx-spectrum | 1982
(5 rows)
rows visited: 5
```],
)

== Selection on attributes

A condition can now name any field. The planner checks its literal against the field's domain, so
`WHERE year = '1982'` is an error and not a query that returns nothing:

#output(title: "$ sql DIR \"SELECT * WHERE year = '1982'\"")[```
error: year must be an integer; '1982' isn't
```]

On the key, the leaf is the `Get` or `RangeScan` of @ch-sql. On anything else there's no index to
ask, so the plan is a `Filter` over a `FullScan`:

#output(title: "$ sql DIR \"SELECT key, maker WHERE year = 1982\"")[```
plan:
  Project key, maker
    Filter year = 1982
      FullScan (fold: every row)
 key         | maker
-------------+-----------
 c64         | commodore
 zx-spectrum | sinclair
(2 rows)
rows visited: 5
```]

The two conditions look alike in SQL and cost very different amounts. On a table of a million rows,
the same layer answers each five times:#footnote[The program is
#repo("book/code/ch04/src/bin/time-where.rs"). Each time is the median of five runs, on a machine that
was doing other work too, so only the ratio's order of magnitude means anything.]

#table(
  columns: (1fr, auto, auto, auto),
  [Query], [Rows returned], [Rows visited], [Time],
  [`SELECT * WHERE key = 'key-0500000'`], [1], [1], [1.8 µs],
  [`SELECT * WHERE year = 1982`], [33,333], [1,000,000], [1.33 s],
)

The attribute query reads and decodes every record in the store to return one in thirty. The
fix is an index on `year`, which is one more thing every `INSERT` has to keep up to date, and
that's @ch-secondary.

That's a row with attributes: a schema above the engine, a few bytes of encoding, and a batch that
makes `INSERT` mean what it says. The engine underneath still stores a key and a value.

#takeaways(
  [A _domain_ is the set of values an attribute can take; a _schema_ lists the attributes and their
   domains.],
  [The engine doesn't change: a row is encoded into the bytes of a value by a layer above it.],
  [Tags in the encoding let a wrong schema fail instead of decoding nonsense.],
  [`INSERT` isn't `put`: it fails when the key exists, because the key identifies the tuple.],
  [A `get` followed by a `put` races; a batch with `Cond::Absent` doesn't.],
  [A value outside its domain is refused before anything is written, and every field must have a
   value.],
  [A condition on the key is a lookup; a condition on an attribute reads every record.],
)

#todo(
  [Store the schema in the store: `CREATE TABLE` and a catalog, when a second table needs them.],
  [What happens to stored rows when a schema changes.],
  [The cost of decoding, measured apart from the cost of reading.],
)
// chapter-status:begin
// stage: early-draft
// revision: 1
// body: 748a28610d3a
// activity: 2026-11-22 | r1 | stage | yabiir | note | migrated from Incipient; text complete, not reviewed by the author
// chapter-status:end
