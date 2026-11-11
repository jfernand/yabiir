#import "../template/isss.typ": *

= Bags and Nulls <app-bags>

#status[Incipient. Every claim is sourced and every SQL output comes from a real SQLite run; the
other vendors' behaviour is from their documentation, not from runs.]

Codd's relational model has two rules that SQL broke, and the rest of the field has paid for them
since: a relation is a _set_, so it never holds the same tuple twice, and every attribute of every
tuple has a value @codd1970. SQL tables are _bags_ — multisets — and any value in them can be `NULL`.
This appendix uses SQL's own words, tables, rows and columns, because it's about SQL as shipped.
#footnote[All the SQL below ran in SQLite 3.53.4 from #repo("book/code/appA/run.sh"); the output is
in #repo("book/code/appA/bags-and-nulls.txt").]

I think both were mistakes, and I'm far from the first: Codd counted duplicate rows among SQL's
"fatal flaws" @codd1989flaws, C.J. Date took SQL apart in 1984 @date1984critique, and his _SQL and
Relational Theory_ gives the subject a chapter whose title is the whole argument: "No Duplicates, No
Nulls" @date2015sql.

== Duplicate rows

SQL has allowed duplicate rows from the start, and the standard example in their defence is a
cash-register receipt @celko2005: buy two bottles of milk and the receipt has two identical lines.
David Beech used the same example in a letter to _Datamation_ @celko2005. Here's the receipt:

```sql
CREATE TABLE receipt (item TEXT, price INTEGER);
INSERT INTO receipt VALUES ('milk', 120), ('milk', 120), ('bread', 250);
```

It has three rows and two distinct ones, and the difference is money: `sum(price)` is 490, and over
`SELECT DISTINCT *` it's 370.

"But wait," says the attentive reader, "that's the point: the duplicate is real." The two purchases
are real. The duplicate is an artifact: the table dropped the one fact that tells the two bottles
apart — which line of the receipt each one is. Give the table that column, make it the key, and the
rows are distinct, the sums are right, and nothing needs a multiset. A yabiir store can't hold the
bad receipt at all: no two records share a key (@ch-select), so its records are a set.

== What bags cost

A bag makes the number of rows in an answer depend on how the question was written, not only on what
it asks @date2015sql. Here are two ways to ask which computers went on sale in a year with a recall,
against a `computers` table with a primary key and a `recalls` table that lists 1981 twice:

```sql
SELECT count(*) FROM computers WHERE year IN (SELECT year FROM recalls);    -- 4
SELECT count(*) FROM computers c JOIN recalls r ON c.year = r.year;         -- 6
```

Four computers, six rows. Neither table holds a duplicate computer; the join manufactured two. The
usual fix is a `DISTINCT`, and every `DISTINCT` costs a sort or a hash table.

The optimizer pays too. Most of what a query optimizer does is replace a query with a cheaper one that
returns the same answer, and "the same answer" over bags is a much harder question than over sets.
Surajit Chaudhuri and Moshe Vardi showed in 1993 that the classic set-semantics optimization of
conjunctive queries — removing a redundant condition — is no longer possible under bag semantics
@chaudhuri1993. Whether one conjunctive query is contained in another under bag semantics, the question
underneath every such rewrite, is still open: nobody knows if it's even decidable, and with
inequalities it isn't @jayram2006. Under set semantics the same problem has been solved since 1977:
Ashok Chandra and Philip Merlin showed it NP-complete, decided by looking for a homomorphism between
the two queries @chandra1977.

== NULL

The vendors didn't invent `NULL`; Codd did. His 1979 paper added a null marker for "value unknown"
and a third truth value, _unknown_, for comparisons against it @codd1979. By 1990 he wanted two kinds
of null — "missing but applicable" and "inapplicable" — and a four-valued logic to go with them
@codd1990. SQL took the 1979 version, one `NULL` and three truth values, and kept it.

Three-valued logic is where `NULL` stops being a missing value and becomes a trap. `NULL = NULL` isn't
true, and `NULL <> NULL` isn't true either: both are `NULL`. A `WHERE` clause keeps only the rows for
which its condition is true, so a `NULL` anywhere in a comparison quietly drops the row:

```sql
SELECT count(*) FROM computers WHERE year NOT IN (SELECT year FROM recalls);   -- 1
INSERT INTO recalls VALUES (NULL);
SELECT count(*) FROM computers WHERE year NOT IN (SELECT year FROM recalls);   -- 0
```

One unknown recall year, and the Apple II — 1977, no recall anywhere near it — vanishes from the
answer. `NOT EXISTS` still returns it; jOOQ's manual lists `NOT IN` under "don't do this" for exactly
this reason @jooq-notin.

The trap isn't a corner case. Leonid Libkin showed that the answers SQL's three-valued logic returns
can be unrelated to the answers that are true whatever the missing values turn out to be — the
_certain answers_ @libkin2016. Paolo Guagliardo and Libkin then proved that the third truth value buys
no expressive power: every query in their SQL fragment can be evaluated with ordinary two-valued logic
@guagliardo2017. All of the confusion, none of the power.

Tony Hoare called his null reference, added to ALGOL W in 1965 "simply because it was so easy to
implement," his billion-dollar mistake @hoare2009. SQL's `NULL` is a different null with the same
disease: one special value that every operation has to remember to handle, and doesn't.

== NULL, vendor by vendor

Where the standard leaves a behaviour implementation-defined, each vendor defines it its own way.

- *Unique columns.* SQLite accepts three `NULL`s in a `UNIQUE` column, because no `NULL` equals
  another. SQL Server allows one per column @mssql-unique. SQL:2023 added `UNIQUE NULLS [NOT]
  DISTINCT` so a schema can finally say which it means, and left the default implementation-defined
  @winand-unique.
- *Empty strings.* Oracle treats a character value of length zero as `NULL` @oracle-nulls. Its 10g
  documentation said it did so "currently," that this "may not continue to be true in future releases,"
  and recommended not treating empty strings as nulls @oracle-nulls-10g. Seven major releases later,
  the 26ai documentation still says it does @oracle-nulls.

The same `CREATE TABLE` and the same `INSERT`s give different answers on different engines, and an
application that moves between them inherits every difference.

== Document databases

In the 2000s a generation of developers left SQL for document databases, MongoDB the most popular of
them. Michael Stonebraker and Andrew Pavlo record the two messages that sold them: "SQL and joins are
slow," and ACID transactions are unnecessary @stonebraker2024. Neither message is about bags or
nulls. My own reading is that they didn't need to be: anyone who'd been bitten by a `NOT IN` returning
nothing, or by an Oracle empty string turning into `NULL`, already had a reason to listen.

The ending is on record. MongoDB added multi-document ACID transactions in version 4.0, in 2018
@mongodb2018. "Despite strong protestations that SQL was terrible, by the end of the 2010s, almost
every NoSQL DBMS added a SQL interface. … The last holdout was MongoDB, but they added SQL for their
Atlas service in 2021" @stonebraker2024. MongoDB came back to SQL, and the bags and the nulls were
waiting for it.

That's the case against bags and nulls: sets and values, as Codd first wrote them.

#takeaways(
  [SQL tables are bags: the same row can appear twice, and the count of an answer depends on how the
   query was written.],
  [Duplicates come from a missing key; give the table its key and the multiset is unnecessary.],
  [Bag semantics blocks optimizations that set semantics allows; bag containment of conjunctive
   queries is still open.],
  [`NULL` brings three-valued logic, under which `NOT IN` with one `NULL` returns nothing.],
  [Vendors disagree on `NULL` in unique columns and on empty strings, and the standard lets them.],
)

#todo(
  [Run the examples on PostgreSQL, SQL Server and Oracle, and add their outputs beside SQLite's.],
  [The 1974 SEQUEL paper, for what SQL's designers wrote at the time about duplicates.],
  [Codd's "fatal flaws" in his own words, from the 1989 article.],
)
