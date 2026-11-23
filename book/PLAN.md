# Plan — *Append Only*

The book goes one step at a time. Each step adds one relational operator, or one piece of DML, and
the **smallest change to yabiir's API** that can run it, the way Chapter 0 runs `SELECT *` with
`open` and `fold` alone. Nothing arrives before something needs it: no tables before there are two
relations, no structured values before a query needs a second attribute, no index before a query is
too slow without one.

Scope is Codd's: the algebra of *Relational Completeness of Data Base Sublanguages* (1972) — union,
intersection, difference, Cartesian product, restriction, projection, join, division — plus the
calculus it is equivalent to, and set-at-a-time insert, update and delete. Set semantics throughout,
no `NULL` (Appendix A says why). What SQL added on top for commercial reasons is listed at the end, as
out of scope.

Open decisions are marked **Decision:**.

---

## The sequence: reads and writes, woven

*(Author's decision, 2026-11-21.)* There's no separate DML part. Each command arrives with the new
thing its implementation needs, next to the query that needs it: `INSERT` with attributes and domains,
`DELETE` and `UPDATE` once rows have fields, `INSERT … SELECT` with the second table, foreign keys with
the join. Step numbers are stable names (D1–D5 were the old DML part); the order below is the order of
the book.

Codd's rule for writes is the same as for reads: they're *set-at-a-time* — insert, update and delete
operate on relations, not on one record at a time (Rule 7 of his 1985 twelve rules, "high-level
insert, update, and delete"). The sequence starts one record at a time anyway, because that's what
the engine does, and reaches the set-at-a-time form at D2 and D4.

### 1. π — Projection *(Chapter 0, done)*

- **What:** keep some attributes of a relation, drop the rest. `SELECT value`.
- **Codd:** 1970 and 1972. Removes duplicate tuples: the result is a set.
- **yabiir needs:** `open`, `fold`.
- **Status:** Chapter 0 runs `SELECT *` ≡ π<sub>key,value</sub>(σ<sub>true</sub>(store)), and
  introduces σ alongside π, with the condition every row satisfies. The store is one table with fields
  `key` and `value`.
- **Decision:** the first projection that drops the key (`SELECT value`) is where duplicates first
  appear. Codd's π returns a set there; the book should implement that, not SQL's bag.

### 2. σ — Restriction (selection), on the key

- **What:** keep the tuples that satisfy a condition. `WHERE`. Codd called it *restriction*; textbooks
  say *selection* and write σ.
- **The step:** the first condition that drops rows, and the obvious one: `SELECT * WHERE key =
  'zx-spectrum'`. (σ<sub>true</sub> is already in Chapter 0.)
- **yabiir needs:** `get`. σ<sub>key = k</sub> is one keydir lookup instead of a fold over everything.
- **Status:** Chapter 1, *Selection by Key*, done (2026-11-11). It absorbed the old keydir chapter:
  the entry format, the keydir and hash indices, plus both translations measured on a million rows.

### 3. σ — Restriction, on ranges of the key

- **What:** `WHERE key BETWEEN 'a' AND 'c'`, `WHERE key LIKE 'zx-%'` (prefix), and the comparison
  operators < ≤ > ≥ on the key.
- **yabiir needs:** an ordered keydir and `scan(range)` — the `BTreeMap` change, `yabiir-btree`.
- **Old material:** the current Chapter 2 (sorted indices, range scans) belongs here.
- **Status:** yabiir 0.6.0 has the ordered keydir (`KeyOrder::Sorted`) and `Engine::scan`, and 0.9
  `Engine::scan_page` (a page of a range, either direction);
  Chapter 2 (sorted indices) explains it, and Chapter 3 ("From SQL to get/scan") parses
  `BETWEEN` and plans it as a range scan.

### 3a. From SQL to get/scan *(Chapter 3, incipient)*

- **What:** what happens between the text of a query and the calls on the store: parse → AST →
  plan → execute. Parsing waved at; the tree, a rule-based planner and a push executor walked
  through, with `EXPLAIN`-style plans from real runs.
- **yabiir needs:** the `yabiir-sql` crate: `parse`, `Select`, `plan`, `Plan`, `execute`.
  Grows with each step: every new piece of grammar adds AST variants, plan operators and rules.

### 4. Values with attributes — the record gets structure

Not an operator, but π and σ need it to mean anything beyond one value.

- **What:** a value holds several attributes — (key, name, maker, year, price) — so `SELECT maker,
  year` is a real projection and `WHERE year = 1982` is a restriction on something other than the key.
- **Codd:** *domains*: every attribute draws its values from a named domain (a type). This is where
  domains come in, and where a value that doesn't fit its domain is rejected.
- **yabiir needs:** a value encoding — a codec in a layer above the engine; the engine is unchanged.
  This is the "turn the value into a record" question, answered at the point the book needs it.
- σ on a non-key attribute is a full `fold` again. That's deliberate: it sets up step 10.
- **Status:** Chapter 4, *Attributes and INSERT*, drafted (2026-11-21), with D1. `yabiir-sql`'s `table`
  module has the schema, the codec, the parser, the planner and `INSERT`; the schema is given by the
  program, and the catalog arrives with the second table (step 5).

### D1. `INSERT` — one tuple

- `INSERT INTO computers VALUES ('amiga', 'commodore', 1985)`.
- **The point:** `INSERT` and `put` are not the same. `put` overwrites; `INSERT` must *fail* if the key
  exists — the primary key is the identity of the tuple, and a relation can't hold two with the same
  key (*entity integrity*, Codd).
- **yabiir needs:** an insert-if-absent. yabiir 0.9 has it: a `write_batch` of
  `Op::Check { cond: Cond::Absent }` and the `Op::Put`. The book shows first why `get` then `put`
  races, then the batch that doesn't.
- **Status:** in Chapter 4, because a row with attributes needs something that writes it. The race is
  measured (`book/code/ch04/src/bin/insert-race.rs`).

### D2. `DELETE` — by key, then by condition

- `DELETE FROM computers WHERE key = 'c64'` → one tombstone.
- `DELETE FROM computers WHERE year < 1980` → σ, then one tombstone per match: the first
  set-at-a-time write.
- **yabiir needs:** `delete`. Where the space goes is `merge` — a natural place for it.

### D3. `UPDATE` — non-key attributes, then the key

- `UPDATE computers SET price = 299 WHERE key = 'c64'` → read, modify, append a new version.
- On an append-only store, every update is a new record and the old one is garbage until `merge`.
- Updating the **key** is a delete plus an insert, and the book says so: the key is the identity.
- **The race:** two concurrent read-modify-writes lose one update. The fix is a compare-and-put:
  read with `get_versioned`, write a batch with `Cond::Version(v)` on the key, retry if the check
  fails (yabiir 0.9). It's the smallest transaction-shaped thing the book needs; multi-statement
  transactions are out of this plan's scope.

### 5. ∪ ∩ − — Union, intersection, difference

- **What:** two relations with the same attributes (*union-compatible*), combined as sets. `UNION`,
  `INTERSECT`, `EXCEPT`.
- **The step:** this is the first operator that needs **two relations**, so it's where *tables*,
  `FROM`, `CREATE TABLE` and the **catalog** enter the book.
- **Tables:** one store, with a table id in front of every key.
- **The catalog** *(author's decision, 2026-11-22)*: one more table in the same store, with a reserved
  id, holding each table's name, id, attributes and domains. `FROM` resolves a name through it, and rows
  are decoded with the schema it records, so a program can no longer bring the wrong one. The database
  describes itself with its own tables (Rule 4 of Codd's 1985 twelve rules). `CREATE TABLE` is a
  `write_batch`: `Cond::Absent` on the name, and a version check on the counter that hands out ids.
  No `ALTER TABLE` and no migrations.
- **yabiir needs:** two ordered scans, merged in one pass: union, intersection and difference of two
  sorted key streams are three variations of the same merge loop.
- **Note:** `UNION` is the set operator. `UNION ALL` is SQL's bag version, and out of scope.

### D4. Set-at-a-time `INSERT … SELECT`

- `INSERT INTO classics SELECT … FROM computers WHERE year < 1983` — relational assignment: the
  result of a query, inserted as a relation. Closes the loop back to Part 1.

### 10. Secondary indices

Not an operator: the fix for step 4's slow σ on non-key attributes.

- **yabiir needs:** an index table per indexed attribute, maintained on every write — in the same
  `write_batch` as the record (0.9), so a crash can't separate them.
- **Status:** Chapter 4 states the costs and, since 0.9, the atomic batch and version check that keep
  an index consistent; the index layer itself isn't built.
- **Placement:** after D2, D3 and tables: an index is one more table, and it has to be maintained on
  insert, update and delete. Its draft is Chapter 5 for now, and moves when those chapters exist.

### 6. × — Cartesian product, and ρ — rename

- **What:** every tuple of one relation paired with every tuple of another. `FROM a, b`.
- **ρ:** both relations have a `key`; the product needs them told apart. Rename isn't in Codd's 1972
  algebra (he addressed attributes by position and domain); textbooks added ρ for exactly this. It
  appears here as a footnote-sized tool, not a section of its own.
- **yabiir needs:** a fold nested inside a fold. Deliberately expensive, as the motivation for join.

### 7. ⋈ — Join

- **What:** σ applied to ×, written as one operator: θ-join (any condition), equi-join (equality), and
  natural join (equality on the attributes the two relations share).
- **The step:** a second relation keyed by the same thing, or by a value of the first — `makers`
  keyed by maker name, joined on the `maker` attribute of `computers`. That's a foreign key, named
  but not enforced yet (step D5).
- **yabiir needs:** nothing new: an index nested-loop join is a `fold` over one side and a `get` on the
  other; a merge join is two ordered scans. The book measures both against step 6's product.

### D5. Integrity constraints — foreign keys

- **Domains** (step 4) and the **primary key** (D1) are enforced from Chapter 4, where they arrive; `UPDATE`
  checks domains too (D3).
- **Foreign keys** (*referential integrity*, Codd): `computers.maker` must name a key of `makers`;
  inserting an unknown maker, or deleting a maker that's still referenced, fails.
- **Decision:** without `NULL`, a foreign key can't be "unknown", so every tuple must reference
  something real. That's the Codd-without-nulls position, and it's the consistent one for this book.

### 8. ÷ — Division

- **What:** the "for all" query: the makers that sold a computer in *every* year of a given set.
- **Codd:** in the 1972 algebra, because without it the algebra can't say "for all" directly.
- **SQL:** has no `DIVIDE`; it's written with a double `NOT EXISTS`, and the book shows both.
- **yabiir needs:** nothing new; it's built from the operators above.

### 9. The relational calculus

- **What:** the same queries said declaratively — "the tuples *t* such that …" — instead of as a
  sequence of operators. Codd's 1972 theorem: the algebra and the calculus are equally expressive
  (*relational completeness*).
- **Why here:** SQL's `SELECT … FROM … WHERE` reads like the calculus and runs like the algebra. Once
  the reader has built every operator, this chapter shows the translation from one to the other —
  which is what a query planner does.

---

## Out of scope

Kept out because they aren't part of Codd's model, or contradict it:

- Bags: `UNION ALL`, `SELECT` without set semantics as the default, multiset aggregates.
- `NULL`, three-valued logic, and everything that exists only to cope with them: outer joins,
  `COALESCE`, `IS [NOT] NULL`, `NULLS FIRST`.
- `ORDER BY`, `LIMIT`/`FETCH FIRST`, `OFFSET`: a relation has no order. The book can mention that
  results get *presented* sorted, as an output step after the algebra.
- `MERGE`, upsert dialects, sequences and identity columns, triggers, stored procedures, window
  functions, recursive queries.

**Decision:** aggregates (`COUNT`, `SUM`, `GROUP BY`). They aren't in the 1972 algebra, but they're
basic enough that a reader will ask. Options: leave them out; or add one short step after division as
an extension, with grouping defined over sets so it doesn't smuggle bags back in.

---

## yabiir's API, step by step

| Step | Query or statement | New in the API |
| --- | --- | --- |
| 1 | `SELECT *` (π) | `open`, `fold` |
| 2 | `WHERE key = k` (σ) | `get` |
| 3 | `WHERE key BETWEEN …` (σ) | ordered keydir, `scan` |
| 3a | text → tree → plan | `yabiir-sql` crate: parser, AST, planner, executor; `Engine::key_order` |
| 4 | attributes in values | value codec (layer above the engine) |
| D1 | `INSERT` | `put`; insert-if-absent as `write_batch` with `Cond::Absent` (0.9) |
| D2 | `DELETE` | `delete` |
| D3 | `UPDATE` | read-modify-write: `get_versioned`, then `write_batch` with `Cond::Version` (0.9) |
| 5 | `CREATE TABLE`, `UNION` / `INTERSECT` / `EXCEPT` | tables (key prefix); catalog as a table; merge of two scans |
| 10 | faster σ on attributes | index tables, maintained in one `write_batch` with the record (0.9) |
| 6–8 | ×, ⋈, ÷ | nothing new |
