#import "../template/isss.typ": *

= Algebra and Calculus <app-calculus>

#status[Incipient. The definitions, one worked example in both languages, safety and Codd's
theorem are in place; the translation from calculus to algebra is only sketched.]

Codd gave the relational model two query languages: an _algebra_, which says how to compute an
answer, and a _calculus_, which says what the answer is @codd1971 @codd1972. Both describe the same
answers, and Codd proved that each can say everything the other can.

== Algebra

An _algebra_ is a set of objects together with operations on them that are _closed_: every operation
takes objects of the set and returns an object of the same set. The integers with `+` and `×` are one:
add or multiply two integers and the result is an integer, which can be added or multiplied again.

In the relational algebra the objects are relations, and the operations are Codd's operators:
selection σ, projection π, union ∪, intersection ∩, difference −, Cartesian product ×, join ⋈ and
division ÷ @codd1972. Each takes one or two relations and returns a relation, so they compose into
expressions:

$ pi_("key", "value") (sigma_"true" ("store")) $

An expression reads inside out, as a recipe: start from `store`, keep the rows that satisfy the
condition, keep these attributes. Change the order of the steps and you have a different expression,
even when it returns the same answer — which is exactly the freedom a query optimizer uses.

== Calculus

In logic, a _calculus_ is a formal system of rules for writing and manipulating statements; the word
comes from the Latin for a small pebble, the kind once used for counting. It has nothing to do with
derivatives. The _predicate calculus_ is first-order logic: statements built from predicates, `and`,
`or`, `not`, and the quantifiers ∀ ("for all") and ∃ ("there exists").

The relational calculus writes a query as a set defined by a formula of the predicate calculus: _the
tuples $t$ such that …_. Codd's version ranges over tuples, the _tuple relational calculus_
@codd1971:

$ { t : t in "store" } $

— every tuple of `store`, which is `SELECT *`. There's no recipe in it, no order of steps: only the
condition the answer has to satisfy. Michel Lacroix and Alain Pirotte later defined a version whose
variables range over attribute values instead of whole tuples, the _domain relational calculus_
@lacroix1977:

$ { (k, v) : "store"(k, v) } $

== One query in both

The difference shows when a query says "for all". Take the oldest computer in the store: the row whose
year is no later than any other row's.

In the calculus that is one quantifier:

$ { t : t in "store" and forall u (u in "store" -> t."value" <= u."value") } $

The algebra has no ∀. It gets there through difference: take every row, and subtract the rows for
which some other row is older. Finding "some other row" means pairing the store with itself, so the
two copies need different names, $s$ and $u$ (the rename operator ρ):

$ "store" - pi_(s."key", s."value") (sigma_(s."value" > u."value") (rho_s ("store") times rho_u ("store"))) $

Both return the Apple II, 1977. The calculus says what the oldest computer _is_; the algebra builds
the set of computers that aren't the oldest and throws them away. Codd included division ÷ in the
algebra for this kind of query, so that "for all" has an operator of its own @codd1972.

== Safety

A formula can describe a set that no database could return. ${ t : not (t in "store") }$ is every
tuple that isn't in the store, which is every possible pair of byte strings except five.
That answer is infinite, and depends on what the possible values are rather than on what's stored.
The calculus is therefore restricted to _safe_ formulas, whose answers are built only from values
that appear in the database. Every algebra expression is safe by construction: its operators only
ever combine the relations they're given.

== Codd's theorem

In 1972 Codd proved that the algebra and the safe calculus are equally expressive @codd1972. Every
algebra expression has an equivalent calculus formula, which is the easy direction. Every safe
calculus formula has an equivalent algebra expression, and Codd gave an algorithm, his _reduction_,
that produces it.

That theorem gave him a yardstick. A query language is _relationally complete_ if it can express
every query the calculus can; the algebra is, by the theorem, and Codd proposed completeness as the
minimum any database language should meet @codd1972. 

Codd was convinced that the calculus was the better of the two for saying what a user wants, and the
same paper says why @codd1972. A calculus query describes the data by its properties. An algebra
query is a sequence of operations, one of many that would produce the same data, and "the properties
of the desired data tend to get hidden in the particular operation sequence … which the user
selects." He had in mind people interrogating a data base from a terminal, with a language that
needed no "programming loops or any other form of branched execution." He didn't expect most of them
to write the calculus itself — "the majority of users should not have to learn either the relational
calculus or algebra" — but because "requesting data by its properties is far more natural," a
calculus-oriented language was the right foundation for the friendlier languages users would type
@codd1972. SQL, designed at IBM by Donald Chamberlin and Raymond Boyce in 1974, became that language
@chamberlin1974.

Translating the calculus into the algebra was the system's job, not the user's, and that was the
point. Codd had opened his 1970 paper with it: "Future users of large data banks must be protected
from having to know how the data is organized in the machine" @codd1970. The databases of the day —
IBM's IMS, which stored records as trees, and the CODASYL network databases — were queried by
programs that followed links from record to record. Charles Bachman, whose IDS was the model for the
CODASYL databases, called his 1973 Turing lecture "The Programmer as Navigator" @bachman1973. The
navigator had to know how the data was stored, because the path through the storage _was_ the
query.

The translation was also where retrieval could be optimized. A calculus query, Codd wrote, "is an
ideal starting point for search optimization," while a system given an algebra expression could only
optimize each operation on its own, "a very limited form of optimization," or try to work out from
the sequence what the user had meant @codd1972. Choosing how to fetch the data — which index, which
order, which file first — had been the programmer's work. System R's optimizer, described by
Patricia Selinger and her colleagues in 1979, made it the system's @selinger1979.

== Where SQL sits

SQL borrows from both. Its `SELECT … FROM … WHERE` names rows and states a condition on them, the way
the calculus does, and `EXISTS` is the calculus's ∃. Its `UNION` and `EXCEPT` are the algebra's ∪ and
−. A database system that runs SQL turns the query into an algebra expression — a tree of operators —
and then rewrites that tree into a cheaper one that returns the same answer.

This book has no planner: every query in it is translated by hand. Each translation is the step a
planner makes, from what the query says to an expression of operators, and from the operators to
calls on yabiir's API.

That's the difference: the calculus states the answer, and the algebra computes it.

#takeaways(
  [An algebra is a set of objects with closed operations; the relational algebra's objects are
   relations, and its expressions are recipes.],
  [A calculus is a system of logical formulas; the relational calculus defines an answer as the set of
   tuples that satisfy one.],
  ["For all" is one quantifier in the calculus, and a difference (or a division) in the algebra.],
  [Only safe formulas are queries; every algebra expression is safe.],
  [Codd's theorem: the algebra and the safe calculus are equally expressive.],
  [Codd preferred the calculus: it states what the user wants, leaves storage to the system, and is
   the starting point for optimizing the retrieval.],
)

#todo(
  [Codd's reduction, step by step, on the oldest-computer query.],
  [The oldest-computer query run for real, as a hand translation against yabiir.],
)
