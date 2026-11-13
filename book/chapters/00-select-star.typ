#import "../template/isss.typ": *

= SELECT \* <ch-select>

#status[Incipient. The query, the table, the two-method API, projection, selection, `DISTINCT`,
one-field projections and order are in place; the outputs are real runs of the hand translations,
`select-star` and `select-project`.]

#epigraph(source: [Matthew 7:7, Vulgate])[Quaerite, et invenietis.]

How does a computer find information? In an infinite number of ways. Arrays, maps, bags, trees,
files, etc. are all different ways of storing and retrieving information in a computer. One of those
ways is a database, a term so pervasive that it has almost no meaning in people's minds; it is just a
nebulous place where data goes.

If you look at the term, you can regain some understanding: a database is a thing, a base, a
foundation, made of data, that supports something. That something is the storage and retrieval of
data. From the beginning, databases were intended to store _granular_ data, not just blobs, with an
understanding that pieces of data are related to each other. People work in departments, so a person
and a department are pieces of data, connected by the department the person works in. A rocket is
made of subsystems, and a subsystem has parts, and parts need to be ordered, or made, and they use
different materials, that cost money, that is spent every month, in different expense categories…
and so on. The other aspect of databases that was assumed from the start is that they lived on mass
storage devices, not in the computer's memory. This was in part because computer memories were
smaller than the volume of data that needed to be handled, but mostly it was because we wanted that
data to stick around, and computer memories are volatile.

So our quest is to look at how data can be stored on disk, and retrieved in a granular fashion, which
we will call _querying_. The core of the book is to shed light on the relationship between how the
data is stored on disk, and how the data is queried. While doing that, I also want to introduce the
mathematical formalisms that underpin modern databases. Those are stable, and have changed little
since they were introduced, which gives them the quality of being foundational.

The simplest query I can think of is `SELECT *`:#footnote[In SQL, the standard query language
@iso9075, which every query in this book is written in.] every field of every row, with no condition
and no order. The simplest granular data store on disk I can think of is a key/value store; in this
book we are using a particular one, yabiir, which is based on Bitcask @sheehy2010bitcask. yabiir is
written in Rust, and the code examples come from it, so they're in Rust too; a reader who doesn't know
the language should be able to follow them with a little effort. This chapter runs the query against yabiir — which has no SQL, no parser and no planner — by showing the
equivalent operations, in essence translating the query by hand into calls on yabiir's API. Let's get
into it:

#syntax(rr-seq(rr-kw("SELECT", new: true), rr-kw("*", new: true)))

The above is a diagram of the grammar that we are using.#footnote[_Grammar_ comes from the Greek
_grammatikē_, the art of letters, from _gramma_, a letter; _syntax_ from _syntaxis_, an arrangement,
a putting together. In computer science a _grammar_ is the set of rules that says how the phrases of
a language are put together, and the _syntax_ of a language is what those rules describe: the shape
its phrases have on the page, before anything is said about what they mean.] It grows with the book:
each time a section adds to the SQL it handles, a diagram like this one shows the whole of it so far,
read left to right along the line, with the new part in amber.#footnote[These are _syntax diagrams_,
or _railroad diagrams_: a fork in the line is a choice, a line that bypasses a box makes it optional.
Niklaus Wirth drew Pascal's grammar this way @jensen1974pascal; SQLite's documentation still draws
its SQL like this, and Douglas Crockford drew JSON's on json.org.]

== The table

A key/value store is that and nothing more: keys, and a value under each one. A yabiir store holds
_records_, and a record is a key and a value — nothing else. Every record has exactly one key, and *no
two records in a store have the same key*: writing a value under a key the store already holds
replaces that record's value.

As a table, then, a store is the obvious one: each record is a row, the key and the value are its two
fields, and the names of the fields are ours to choose. We call them `key` and `value`. There is only
one table, so a query doesn't need a `FROM` to say which. This one holds five home computers, each
under its name, with the year it went on sale as the value:

#table(
  columns: (auto, auto),
  [key], [value],
  [`apple-ii`], [`1977`],
  [`ibm-pc`], [`1981`],
  [`bbc-micro`], [`1981`],
  [`zx-spectrum`], [`1982`],
  [`c64`], [`1982`],
)

`SELECT *` asks for that table: both fields, all five rows.

== The two-method API

yabiir's `Bitcask` trait has two methods that are enough to read the table and show it, and for this
chapter they're the only methods it has:#footnote[The real trait has seven more, starting with `get`,
`put` and `delete`. The rows here were written with yabiir's `kv` tool, which uses `put`, before the
query ran.]

#srcfile("api.rs", lines: ((82, 82), (92, 92), (115, 119), (137, 137)))[```rust
pub trait Bitcask: Sized {
// …
    fn open(dir: impl AsRef<Path>, opts: Options) -> Result<Self>;
// …
    /// Fold `f` over every currently-live `(key, value)` pair, starting
    /// from `init`. `f` is applied over a point-in-time snapshot of the
    /// keydir taken at the start of the call; concurrent writes made during
    /// the fold are not guaranteed to be observed.
    fn fold<A>(&self, f: impl FnMut(&[u8], &[u8], A) -> A, init: A)
»        -> Result<A>;
// …
}
```]

`open` reads the store's files and builds the in-memory index that @ch-keydir puts to use. `fold`
calls `f` once for every record, with its key and its value as two byte slices, and passes along an
_accumulator_: `f` receives the accumulator built so far and returns the next one, and `fold` returns
the last. A row is those two slices; the API has nothing else to hand `f`.

== Relational operators

In terms of the relational algebra,#footnote[Codd defined two languages over relations: an
_algebra_ of operators, where π and σ come from, and a _calculus_ that says the same things
declaratively — "the tuples $t$ such that …". In 1972 he proved them equally expressive
@codd1972. @app-calculus explains the difference.] `SELECT *` is two operations
on the table, which Codd's model calls a _relation_: its rows are _tuples_ and its fields are
_attributes_ @codd1970.

$ "SELECT *" quad = quad pi_("key", "value") (sigma_"true" ("store")) $

=== Projection

A _projection_ keeps some of a relation's attributes and drops the rest @codd1970. It's written π,
with the attributes it keeps as a subscript:

$ pi_(A_1, ..., A_k) (R) = { t[A_1, ..., A_k] : t in R } $

where $t[A_1, ..., A_k]$ is the tuple $t$ with only the attributes $A_1$ to $A_k$ left in it. In SQL,
the list after `SELECT` is the projection, and `*` stands for every attribute of the table. For the
store that's `key` and `value`, so `SELECT *` is $pi_("key", "value")$, which keeps everything.

=== Selection

A _selection_ keeps the tuples that satisfy a condition and drops the rest. It's written σ, with the
condition as a subscript:

$ sigma_C (R) = { t in R : C(t) } $

Codd called it _restriction_ @codd1972; σ and _selection_ are the textbook names. In SQL, the
condition is the `WHERE` clause. `SELECT *` has no `WHERE`, which is the condition every tuple
satisfies, $sigma_"true"$: at this stage, selection keeps all the rows.

=== The translation

The program that answers `SELECT *` is one call to `fold` that performs both operations: the
condition decides which rows to keep, and the push keeps both of a row's fields.#footnote[#repo("book/code/ch00/src/main.rs")
is the full program; it also opens the store read-only and prints the rows.]

```rust
let condition = |_key: &[u8], _value: &[u8]| true;   // σ_true

let rows = store.fold(
    |key, value, mut rows: Vec<(Vec<u8>, Vec<u8>)>| {
        if condition(key, value) {
            rows.push((key.to_vec(), value.to_vec()));   // π_key,value
        }
        rows
    },
    Vec::new(),
)?;
```

Run twice, it returns the same five rows, in a different order each time:#footnote[Both runs are from
#repo("book/code/ch00/run.sh"); its output is in #repo("book/code/ch00/select-star.txt"). The header
and the row count are printed by `select-star` itself.]

#grid(columns: (1fr, 1fr), column-gutter: 10pt,
  output(title: "$ select-star DIR · first run")[```
 key         | value
-------------+-------
 ibm-pc      | 1981
 apple-ii    | 1977
 bbc-micro   | 1981
 c64         | 1982
 zx-spectrum | 1982
(5 rows)
```],
  output(title: "$ select-star DIR · second run")[```
 key         | value
-------------+-------
 bbc-micro   | 1981
 apple-ii    | 1977
 zx-spectrum | 1982
 ibm-pc      | 1981
 c64         | 1982
(5 rows)
```],
)

== DISTINCT

`SELECT DISTINCT` removes the rows that are equal to one another, leaving one of each. Here it makes
no difference. Every row carries its key, no two rows share a key, so no two rows are equal:
`SELECT DISTINCT *` returns the same five rows as `SELECT *`.

#syntax(rr-seq(rr-kw("SELECT"), rr-opt(rr-kw("DISTINCT", new: true)), rr-kw("*")))

Values alone can repeat — the IBM PC and the BBC Micro both went on sale in 1981, and the ZX Spectrum,
the computer I started on, and the Commodore 64 both in 1982 — but only a projection that drops the
key can bring two of them together. Codd's relations are sets, so his π would then merge them; SQL's
tables can hold the same row twice, which is why SQL needs `DISTINCT` at all. That choice, and the
`NULL` that came with it, are the subject of @app-bags.

== Projecting one field

`*` names every field. Naming one field instead keeps that field and drops the other:

#syntax(rr-seq(rr-kw("SELECT"), rr-opt(rr-kw("DISTINCT")),
  rr-choice(rr-kw("*"), rr-kw("key", new: true), rr-kw("value", new: true))))

#block(sticky: true)[The translation is the same `fold`, keeping one field of each row instead
of both:]

```rust
let kept = if keep_key { key } else { value }.to_vec();   // π_key or π_value
rows.push(kept);
```

`SELECT key` is $pi_"key" ("store")$: the five keys, which never repeat. `SELECT value` returns five
rows as well, and two of them say 1981 and two say 1982:#footnote[#repo("book/code/ch00/src/bin/select-project.rs")
is the full program; the runs are from #repo("book/code/ch00/project.sh"), and its output is in
#repo("book/code/ch00/select-project.txt").]

#grid(columns: (1fr, 1fr), column-gutter: 10pt,
  output(title: "$ select-project DIR key")[```
 key
-------------
 apple-ii
 zx-spectrum
 c64
 bbc-micro
 ibm-pc
(5 rows)
```],
  output(title: "$ select-project DIR value")[```
 value
-------
 1981
 1982
 1982
 1981
 1977
(5 rows)
```],
)

That's SQL's answer, not Codd's. His projection returns a relation, and a relation is a set, so the
repeats merge:

$ pi_"value" ("store") = { (1977), (1981), (1982) } $

`SELECT DISTINCT value` is the SQL that gets Codd's answer. To recognise a repeat, the translation
keeps every value it has already returned, in a set, and skips a row whose value is in it:

```rust
if !distinct || seen.insert(kept.clone()) {   // DISTINCT: skip a value already kept
    rows.push(kept);
}
```

#output(title: "$ select-project DIR value --distinct")[```
 value
-------
 1981
 1977
 1982
(3 rows)
```]

That set grows with the result, one entry for every distinct value, and it's the price of
`DISTINCT`. A projection that keeps the key never has to pay it: keys never repeat, so there's
nothing to remove. A planner that knows a field is unique can drop the `DISTINCT` and the set
together.

== Order

`fold` visits rows in the order of the keydir, and the keydir is a hash map whose hash function is
seeded at random each time a map is created. Every `open` builds a new map, so every `open` can return
the rows in a new order, which is what the two runs show.

A relation is a set, and a set has no order @codd1970. SQL makes no promise about the order of a
result without `ORDER BY` @iso9075, so either order is a correct answer. The order of the keydir is a
property of one open store, and a program that depends on it breaks the next time the store is opened.

That's `SELECT *`: a selection that keeps every row, a projection that keeps every field, and no
order. @ch-keydir adds the first condition that drops rows, and the index that answers it.

#takeaways(
  [yabiir has no SQL: a query is translated by hand into calls on its API, and the translation runs.],
  [A record is a key and a value, nothing else; no two records in a store share a key.],
  [The store is one table with two fields we name, `key` and `value`; `SELECT *` returns both.],
  [`open` and `fold` are enough to read the table and show it.],
  [`SELECT *` is $pi_("key", "value") (sigma_"true" ("store"))$: every field, every row.],
  [`DISTINCT` changes nothing while rows carry their keys, because keys never repeat.],
  [`SELECT value` returns one row per record, repeats included; `SELECT DISTINCT value` returns
   Codd's $pi_"value"$, a set, and needs memory for every distinct value to do it.],
  [Without `ORDER BY`, any order is correct, and yabiir's changes with each open.],
)

