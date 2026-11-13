#import "../template/isss.typ": *

= From SQL to get/scan <ch-sql>

#status[Incipient. Parsing is waved at; the tree, the planner and the executor are real, in yabiir's
`sql` feature, and the outputs are real runs of `sql`, the chapter's program.]

#epigraph(source: [Chamberlin and Boyce, 1974, query Q1 @chamberlin1974])[`SELECT NAME FROM EMP
WHERE DEPT = 'TOY'`]

In @ch-select, `SELECT DISTINCT value` became a `fold` that kept each row's value and skipped the
ones it had already seen. In @ch-keydir, `WHERE key = 'zx-spectrum'` became a `get`. I wrote both
translations by hand. A database makes them itself, from the text, in three steps:

#align(center, block(inset: (y: 4pt), text(font: font-mono, size: 8.6pt)[
  text #h(6pt) →#sub[parse] #h(6pt) tree #h(6pt) →#sub[plan] #h(6pt) plan #h(6pt) →#sub[execute] #h(6pt) rows
]))

The _parser_ reads the text and builds a tree of what the query says: the _abstract syntax tree_,
or AST. The _planner_ turns the tree into a _plan_: which of the store's operations to call, and
what to do with the rows they return. The _executor_ runs the plan. yabiir has all three now, behind
its `sql` feature, for the queries this book has reached, plus one: `BETWEEN`, the range that
@ch-sorted's sorted keydir is for.

#syntax(rr-seq(rr-kw("SELECT"), rr-opt(rr-kw("DISTINCT")),
  rr-choice(rr-kw("*"), rr-kw("key"), rr-kw("value")),
  rr-opt(rr-seq(rr-kw("WHERE"), rr-kw("key"),
    rr-choice(
      rr-seq(rr-kw("="), rr-ph("string")),
      rr-seq(rr-kw("BETWEEN", new: true), rr-ph("string", new: true), rr-kw("AND", new: true),
        rr-ph("string", new: true)))))))

`key BETWEEN 'a' AND 'c'` holds for the keys from `'a'` to `'c'`, both ends included.

== Parsing

Parsing is a subject of its own, and this chapter waves at it. The parser cuts the text into
_tokens_ — words, strings in quotes, `*` and `=` — and reads them with one function for each rule of
the grammar in the diagram: `select` calls `fields`, then `condition` if it finds a `WHERE`, and each
function either consumes the tokens it expects or stops with an error that says what it expected and
where.#footnote[About 340 lines, a hundred of them tests, in #repo("crates/yabiir/src/sql/parse.rs").
This is a _recursive-descent_ parser, the kind people write by hand; a grammar this small needs no
generator.] A condition on the value is a field the grammar doesn't allow there yet:#footnote[Every
run in this chapter is from #repo("book/code/ch03/run.sh"), on Chapter 0's five computers; its
output is in #repo("book/code/ch03/sql.txt"). The program is #repo("book/code/ch03/src/main.rs").]

#output(title: "$ sql DIR \"SELECT * WHERE value = '1982'\"", size: 6.5pt)[```
syntax error at byte 15: expected key (conditions on the key only, so far), found VALUE
  SELECT * WHERE value = '1982'
                 ^
```]

== The tree

The tree keeps what the query says and forgets how it was spelled: `select distinct VALUE` and
`SELECT DISTINCT value` are different text and the same tree. Here is all of it:#footnote[From
#repo("crates/yabiir/src/sql/ast.rs"), doc comments shortened.]

```rust
pub struct Select {
    pub distinct: bool,                  // SELECT DISTINCT
    pub fields: Fields,                  // the projection
    pub condition: Option<Condition>,    // the WHERE clause: the selection
}

pub enum Fields { All, Key, Value }     // *, key, value

pub enum Condition {
    KeyEquals(Vec<u8>),                  // WHERE key = 'k'
    KeyBetween(Vec<u8>, Vec<u8>),        // WHERE key BETWEEN 'low' AND 'high'
}
```

The program prints the tree before it does anything else with it:

#output(title: "$ sql DIR \"SELECT DISTINCT value\"")[```
tree: Select { distinct: true, fields: Value, condition: None }
```]

Each piece of the tree stands for one operation of the algebra, and the planner turns each into one
operator of the plan. Which operator, for the selection, depends on how the store keeps its keys:

#table(
  columns: (auto, auto, auto, 1fr),
  [Tree], [Algebra], [Plan], [yabiir call],
  [`condition: None`], [$sigma_"true"$], [`FullScan`], [`fold`],
  [`KeyEquals(k)`], [$sigma_("key" = k)$], [`Get(k)`], [`get`],
  [`KeyBetween(a, b)`, sorted], [$sigma_(a <= "key" <= b)$], [`RangeScan(a, b)`], [`scan`],
  [`KeyBetween(a, b)`, hashed], [$sigma_(a <= "key" <= b)$], [`Filter` over `FullScan`], [`fold`, testing each key],
  [`fields: All`], [$pi_("key", "value")$], [nothing], [—],
  [`fields: Key` or `Value`], [$pi_"key"$ or $pi_"value"$], [`Project`], [—],
  [`distinct: true`], [a set, not a bag], [`Distinct`, or nothing], [—],
)

`*` keeps every field, so its projection changes nothing and the plan leaves it out.#footnote[The
first SQL, SEQUEL, projected the way the algebra does: without a `WHERE`, `SELECT DEPT FROM EMP`
"returns a projection, or list of unique values" @chamberlin1974. SQL as it was standardized keeps
the duplicates unless the query says `DISTINCT`.] The last row is
a decision the planner makes on its own, and the next section shows when it chooses _nothing_.

== The plan

A plan is a tree too, of _operators_. The leaves read the store, one way each; the nodes above them
reshape the rows the leaves produce:#footnote[From #repo("crates/yabiir/src/sql/plan.rs").]

```rust
pub enum Plan {
    FullScan,                       // every row: one fold
    Get(Vec<u8>),                   // at most one row: one keydir lookup
    RangeScan(Vec<u8>, Vec<u8>),    // the rows from low to high, in key order
    Filter(Condition, Box<Plan>),   // the input's rows the condition holds for
    Project(Fields, Box<Plan>),     // one field of each of the input's rows
    Distinct(Box<Plan>),            // the input's rows, each once
}
```

The planner builds one from the bottom up. In pseudocode, all of it:

```text
plan(select, order):
    p ← match select.condition
          none                 → FullScan
          key = k              → Get(k)
          key BETWEEN a AND b  → RangeScan(a, b)          if order is sorted
                                 Filter(cond, FullScan)   otherwise
    if select.fields ≠ *:
        p ← Project(select.fields, p)
    if select.distinct
       and select.fields doesn't include key
       and select.condition isn't key = k:
        p ← Distinct(p)
    return p
```

The real one, in `plan.rs`, is the same three steps in thirty lines of Rust. The selection picks the
leaf, because it decides which rows to read. The projection goes on top. `Distinct` goes on last,
and only where it can remove a row. @ch-select showed that a result that keeps the key has no two
equal rows, and a `Get` returns at most one row; in both cases, `Distinct` would have nothing to
remove, so the planner leaves it out:

#grid(columns: (1fr, 1fr), column-gutter: 10pt,
  output(title: "$ sql DIR \"SELECT DISTINCT value\"")[```
plan:
  Distinct
    Project value
      FullScan (fold: every row)
 value
-------
 1982
 1981
 1977
(3 rows)
```],
  output(title: "$ sql DIR \"SELECT DISTINCT key\"")[```
plan:
  Project key
    FullScan (fold: every row)
 key
-------------
 ibm-pc
 zx-spectrum
 c64
 bbc-micro
 apple-ii
(5 rows)
```],
)

A plan prints root first, the way SQL databases print theirs for `EXPLAIN`. Read from the bottom up,
it's the program of @ch-select: a `fold`, keeping each row's value, skipping the values already kept.

== Running a plan

The executor pushes rows up the tree. A leaf calls the store and hands each row it gets, as a key
and a value, to its parent; each parent does its one thing to the row and hands the result to its
own parent, and the root's rows are the answer. Only `Distinct` remembers anything: the set of rows
it has already passed on, the price @ch-select put on `DISTINCT`.

```rust
Plan::Filter(condition, input) => run(input, store, &mut |row| {
    if condition.holds_for(&row[0]) {   // row[0] is the key
        emit(row);
    }
}),
Plan::Distinct(input) => {
    let mut seen = HashSet::new();
    run(input, store, &mut |row| {
        if seen.insert(row.clone()) {   // false if the row was there already
            emit(row);
        }
    })
}
```

#block(sticky: true)[A lookup is the plan @ch-keydir argued for, and `DISTINCT` on it goes, since
one row can't repeat:]

#output(title: "$ sql DIR \"SELECT DISTINCT value WHERE key = 'zx-spectrum'\"")[```
plan:
  Project value
    Get 'zx-spectrum' (get: one lookup)
 value
-------
 1982
(1 row)
```]

A range is where the store's order decides. Opened hashed, the planner has no range to ask for: it
visits every row and tests each key. Opened sorted, it asks the keydir for the range:

#grid(columns: (1fr,), row-gutter: 4pt,
  output(title: "$ sql DIR \"SELECT * WHERE key BETWEEN 'a' AND 'c'\"")[```
plan:
  Filter key BETWEEN 'a' AND 'c'
    FullScan (fold: every row)
 key       | value
-----------+-------
 bbc-micro | 1981
 apple-ii  | 1977
(2 rows)
```],
  output(title: "$ sql DIR \"SELECT * WHERE key BETWEEN 'a' AND 'c'\" --sorted")[```
plan:
  RangeScan 'a' .. 'c' (scan: sorted keydir)
 key       | value
-----------+-------
 apple-ii  | 1977
 bbc-micro | 1981
(2 rows)
```],
)

Both are right, and they differ in what they cost and in their order. The scan visits five rows to
return two; the range visits two, and returns them in key order, because that's the order it walks.
Neither returns `c64`: it starts with `c` but sorts after `'c'`, the way `"alice"` sorts after `"al"`,
so it's outside the range.

#deep(title: "Rules and costs")[
  This planner follows rules, and each rule is right every time: a lookup is never slower than a
  scan, a range on a sorted keydir is never slower than a scan of the whole store. A real planner has
  choices where no rule is always right — which of two indices to use, which of two tables to read
  first — and it chooses by estimating what each plan would cost, from statistics about the data.
  That's _cost-based_ planning, and IBM's System R did it first @selinger1979.
]

That's the step between `SELECT DISTINCT value` and the `fold`: the parser makes a tree of what the
query says, the planner makes a plan of how to answer it, and the executor runs the plan, calling
`fold`, `get` or `scan` at the leaves. @ch-secondary needs a plan with more than one leaf.

#takeaways(
  [A query goes from text to a tree (parsing), from the tree to a plan (planning), and from the plan
   to rows (execution).],
  [The tree says what the query asks, and nothing about how to answer it.],
  [The selection picks the plan's leaf: `fold` for every row, `get` for one key, `scan` for a range
   on a sorted keydir, or a `fold` with a filter on a hashed one.],
  [The projection and `DISTINCT` go above the leaf; the planner drops `DISTINCT` where it can't
   remove a row.],
  [Rows flow up the plan one at a time; only `Distinct` keeps state.],
  [yabiir's planner is rule-based; real planners compare estimated costs.],
)
