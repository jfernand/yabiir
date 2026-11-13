# Voice and style guide — *Append Only*

> Borrowed from the *JVM Internals* book (`/home/javier/java-internals/STYLE.md`, as of 2026-11-10).
> §§1–6 are its voice rules, kept as written (their examples come from that book); the
> JVM-specific wording is swapped for storage. §§7–8 are rewritten for this book.

Derived from the author's blog (`~/jotafernand/content/posts`). The posts this was calibrated against most closely: *Multitasking, From Scratch* (019), *Yet Another Bitcask* (018), *laplink-p2p* (016), *A Computer, at 10,000ft* (019), *Proofs Are Programs* (024), *Solving Centenarian Problems* (025), *Why Is Software So Hard* (013).

This book, like *JVM Internals*, is the blog's technical voice, a notch more disciplined: the precision of 018/019, with the restraint of 024/025, and the humour of the earlier posts used as seasoning, not as the meal.

---

## 1. Who is talking

- **"We", author and reader together.** *(Author's call, 2026-11-06; applies from Appendix A and Chapter 2 onward. Chapter 1 stays in the first person as written.)* The book's default perspective is a shared one: we run the experiment, we dump the bytes, we read the source. "We'll take this apart in Chapter 6." "Let's look at what ends up in the code cache." It is the *we* of two people at one terminal, never the royal or academic *we* ("we posit", "it can be shown that we…").
- **"I" only where it is genuinely the author's.** An opinion, a confession, a mistake made while researching, a true anecdote: "I have to confess I first assumed…", "I was wrong, and I am glad." Keep these rare and short; the reader should feel accompanied, not lectured.
- **"Let's" for the guided tour.** Still the natural opener for a walkthrough: "Let's read it." "Let's call this a CALL instruction."
- **Peer to peer.** The reader is a working senior developer. No hand-holding, no "simply", no condescension. Assume they know what a hash map is; don't assume they know what a tombstone is.
- **Self-deprecating, never self-flagellating.** "a middling software builder", "the worst way of learning how to program, but the best thing in the world". One per chapter is plenty.

## 2. How a section moves

The blog's posts follow a recognisable arc; chapters and long sections should too:

1. **A hook with a person in it.** A motivating observation, a confession, a small surprise. ("I came across *Green Threads from Scratch*…", "I have to confess at not having spent much time hands-on with storage.") In this book the hook is usually *something the storage engine does, or can't do* — a real measurement, a real file on disk, or a real requirement from a program built on yabiir.
2. **Pin down terms — and admit the hand-waving.** Define the thing, then say where the definition is loose: "You get the idea. Also, in case I'm not being clear: the above is very, very hand-wavy."
3. **"For completeness, let's specify what this thing does."** A crisp bullet list of behaviour before the internals.
4. **Show the real artifact.** Hexdumps, real log lines, real flag output, real source. Never a made-up example where a real one is available. Introduce it plainly: "Here's the literal file for `put("lang", "rust")`:"
5. **Walk the artifact field by field.** "Twenty-four bytes: `d5cb411e` is the CRC32, `0019b56a` is the timestamp…"
6. **Land it with a short summary sentence.** "Fourteen `mov`s and a `ret`." "That's it." "It's seven registers and a disciplined `ret`."
7. **Be honest about the edges.** What isn't covered, what was wrong the first time, what's still uncertain. "I failed initially to add this CRC… which in part is why I go through this."
8. **Point forward.** "That's the next chapter." "Stay tuned" is a blog-ism — in the book, a forward reference to a chapter.

## 3. Sentence-level habits

- **Em dashes everywhere** — for asides, for the turn in a sentence, for the punchline.
- **Parentheticals** for the wry aside: "(Claude checked, 'cause ain't nobody got time for that.)" In the book: lighter, but keep them.
- **Short sentences after long ones.** A dense, clause-heavy explanation followed by a four-word verdict. "It just jumps."
- **Italics for the key word** in a definition or the stressed word in a sentence: "a *cooperative* scheduler", "*caller*-saved".
- **Bold for the one idea the section rests on**, at most once per section: "**never overwrite anything. Ever.**" (In Typst that is single stars: `*like this*`.)
- **Anticipate the objection, out loud:** "'But wait!', some attentive reader might interject…" / "The astute reader might notice an apparent problem…" Then answer it.
- **Name the people.** Credit engineers by name with evident respect (antirez, Tao, Harper; here: Sheehy and Smith for Bitcask, Bayer and McCreight for the B-tree, Bloom, Guttman…). Credit is part of the voice.
- **Colloquial seasoning, used sparingly:** "kinda", "work-ish", "for fun and profit", "suspenders and belt", "takes the whole ship with it", "jazzhands". At most a few per chapter, and never in a sentence that carries a technical claim the reader must parse precisely.
- **Mild irreverence about authority and documentation** — the paper that leaves out the obvious ("_yawn_"), the dated glossary. Affectionate, not snarky.
- **Concrete numbers over adjectives.** "about 19 milliseconds", "563 of 598 classes", "twenty-four bytes" — not "very fast", "most classes".

## 4. Headings

*(Author's correction, 2026-11-06; replaces the earlier "phrase, not a label" guidance.)*

- **A heading names what the section is about**, using the term the literature or yabiir's own source uses where there is one (in *JVM Internals*: the spec, the JEP, HotSpot): "Hidden classes" (JEP 371), "The erroneous state" (JVMS §5.5), "Dynamic archives" (JEP 350), "Bytecode quickening" (HotSpot's term), not "The classes nobody wrote" or "It rewrites its own bytecode".
- **Test it against the section.** Does the section talk about the thing the heading names? "Not a loop" passes (the section is about the interpreter not being a loop); "Forty bytes of `iadd`" passes (it reads those forty bytes). A heading that names an incidental characteristic, or a mechanism when the section is about its result, fails.
- **No cleverness at the expense of accuracy.** "The deadlock that isn't one" described a real deadlock; what failed was its detection.
- Chapter titles stay plain enough to find in a table of contents.

## 5. Footnotes

The blog pushes precise-but-digressive material into footnotes (`[^callee-saved]`, `[^why-16]`). Do the same: the body keeps momentum, the footnote carries the exact rule, the edge case, or the war story about the bug.

## 6. What *not* to copy

- **Typos and slips** in the drafts ("right" for "write", "showboxes", "preformance") — the voice, not the errors.
- **Opinion-essay register** (013, 007): the virtue-and-management riffs don't belong in a technical book, beyond a sentence of perspective.
- **Over-seasoning.** Early posts stack jokes; later posts (024, 025) are plainer and stronger for it. When in doubt, cut the joke and keep the number.
- **No wisecrack tails on lists.** *(Author's correction, 2026-11-06.)* A list ends when its last item ends. Don't hang a summarizing appositive or a quip off it ("Launcher, class loading, memory layout, the JITs — everything the collectors stand on"), don't smuggle a joke in as the last item ("every flag you've ever pasted from Stack Overflow"), and don't follow a list with a colon-and-wink ("…: that's this whole book"). If the list needs a conclusion, it gets its own sentence, and that sentence says something new.
  - **The test: does the tail illustrate, or decorate?** A tail that gives a concrete example is information, and should be marked as one: "not in how, *e.g.*, G1 marks the heap." The same clause without "e.g." reads as a punchline. Decoration goes; illustration stays, flagged as illustration.
- **Banned phrases.** *(Author's call.)* Never use these, in any form:
  - "you didn't ask for" / "I never asked for" / "nobody asked for" (as an idiom; the literal "the size you asked for" is fine).
- **No teasers, no mystery.** *(Author's correction, 2026-11-06.)* Don't announce that something is interesting, surprising or important; state it. Banned shapes: "matters more than you'd think", "deserves a closer look", "the part that holds a surprise", "X surprises people", "worth knowing", "here's the catch", "with an asterisk", "unsurprisingly", "the two halves are one story". Also banned *(author, 2026-11-07)*: count-teasers that withhold what they count ("Three different numbers describe X, and the gap between them is why…", "Two things are happening"), "for a reason we'll come to", "a story for Chapter N", "this chapter is about both halves of that sentence", and rhetorical questions as openers ("How much does it buy? Less than we expected.", "Why would X bother?"). Define the thing first ("A range of memory is in one of three states:" + the list), then connect it to the example. Open a section with one declarative sentence that states the fact and cites it, then deliver. The model, from the author's own rewrite of §2.1: *"A class is prepared for execution in three different steps @jvms25-5:"* followed directly by the list. Not "the spec describes a class's arrival with three verbs", and not meta-commentary about the source either ("that's the first sentence of Chapter 5, whose title is those three words"): say what happens, cite where it's defined. Likewise, describe a thing by what it is ("one class is synthesised to contain the lambda"), not with a wink ("a class nobody wrote"). A motivating sentence is fine when it carries a real claim ("you can't understand the effect the AOT cache has without knowing what it avoids"); cut it when it only sounds like one ("the two halves are one story"). Technical text needs no build-up.
- **Close a chapter with one phrase, not a recap list.** "That's HotSpot from the outside." Then the forward pointer. Never re-enumerate the chapter's highlights to sound clever ("Nineteen milliseconds, eighteen threads, one VM Thread holding the stop button…") — the takeaways box already does the summarising.
- **Invented biography.** Personal anecdotes must be true. The ones on record: about the age of the C language; started on a ZX Spectrum (1 MHz, 64 kB, BASIC manual in English, a Spanish speaker); loves C, now Rust; fascinated by storage; uses AI agents openly and says so. Everything else — measurements, runs, bugs — must come from what was actually done for the book.

- **No garbage phrases.** *(Author's call, 2026-11-08.)* Every sentence says something about storage or the reader's code. Cut sentences, and parts of sentences, that talk about the text instead: "This walkthrough reads…", "The listings are from…", "It's the reason for this whole walkthrough", "as we saw", "the next listing shows", "let's now turn to", "the rest of this section". Don't open a section or walkthrough by recapping what an earlier one said ("@sec-x showed that…"): state the fact and cite the section. Also cut filler and hedges ("just", "actually", "really", "quite", "a bit", "in practice" when nothing contrasts with practice, "essentially", "note that", "notice", "it's worth"), vague nouns ("thing", "something", "stuff", "the whole X"), and narration of the obvious ("Here the code checks…" before a listing that checks). The tag a listing is pinned to goes in its caption or `#srcfile` header, not in a sentence.
- **A program in the text is shown, not described.** *(Author's call, 2026-11-08.)* Whenever the text introduces a program ("Here is a program", "the test program", "a probe that…", a measurement made with one), show a cleaned-up excerpt of the code that matters, the part the text then talks about, and give the full version's path in the repository in a footnote, `#footnote[The full program is #repo("code/ch19/src/DecodeAlloc.java").]`. Cleaned up means: no imports, no argument parsing or printing boilerplate, no warm-up loops unless they are the point, lines cut with `// …`; never change what the code does.

## 7. Book-specific conventions

- Every claim about yabiir's behaviour traces to its source at a release tag, or to a real run. Version-specific claims carry `#since("0.6")` (yabiir versions); release changes go in `#changed("0.6")[…]`.
- **yabiir has no SQL.** A query in the text is translated by hand into a program that calls yabiir's API; say so, name the program, and show its real command line (`$ select-star DIR`), never a SQL prompt it didn't have. SQL output is quoted only from an engine that ran it (SQLite in Appendix A).
- **A syntax diagram for every new piece of grammar.** When a section adds to the SQL the book handles, follow the first query that uses it with `#syntax(...)` (template: `rr-seq`, `rr-opt`, `rr-choice`, `rr-loop`, `rr-kw` for keywords and punctuation, `rr-ph` for placeholders), drawing the whole grammar so far with the new items `new: true`. Only grammar a chapter actually translates goes in; no `FROM` until there is more than one table.
- Every quoted output — hexdumps, timings, CLI output — comes from a real run, kept under `book/code/<chapter>/` together with the script that produced it and its raw output.
- Layout: tables never break across pages; code never continues across a page turn except from an even (left) page onto the facing odd (right) page. The template enforces both (`spread()` in `template/isss.typ`); after layout changes, check `typst query databases.typ "<code-start>" --field value` against `<code-end>`.
- Tables are illustrative or evidentiary. An evidentiary table says how it was produced (in the text or caption; the script and raw output in a footnote) and defines its labels before it appears. A table uses only terms the text has already introduced.
- References to files in the repository go in footnotes, never in the body: `#footnote[The full run is #repo("book/code/ch01/run.sh").]`.
- Callouts: `note` for asides, `warn` for foot-guns that lose or corrupt data or silently cost performance, `deep` for mechanism detours, `changed(v)` for release changes; `takeaways` closes each chapter.
- Draft scaffolding (this book is incipient): `#status[…]` under a chapter opener says what state it is in; `#todo(…)` at the end lists what is still to write. Both go before publishing.
- Terminology:
  - *keydir* for yabiir's in-memory index (Bitcask's term), *data file* for the on-disk log, *entry* for one record in it, *tombstone* for a delete marker.
  - **One table from Chapter 0.** The store is a table with two fields whose names we fix, `key` and `value`; each record is a row. There is only one table, so queries have no `FROM` until a second relation arrives (the set operators, PLAN.md step 5).
  - *table*, *row*, *field* in SQL terms; *relation*, *tuple*, *attribute* in the relational algebra (π, σ, ⋈), each defined at first use.
  - `SELECT *` returns both fields: it is π<sub>key,value</sub>(σ<sub>true</sub>(store)).
  - A *record* is a key and a value, nothing else. Every record has exactly one key, and no two records in a store share a key, so records never repeat; values can.
  - *index* alone means "a structure that maps a key to where its data is"; qualify it otherwise (*hash index*, *sorted index*, *secondary index*, *inverted index*).
- Complexity: big-O only with its *n* defined ("n keys in the store"), and next to a measured number when one exists.

## 8. Source listings

- Excerpts via `#srcfile(path, lines: ((from, to), ...), notes: ((line, [..]), ...))[```rust ...```]`, pinned to a published release tag (`v0.5.0` by default; up to that tag sources are under `src/`, after the workspace split under `crates/yabiir/src/`, passed as `root:`).
- Edits allowed: cut lines (a line that is exactly `// …`), condense alignment padding, wrap long lines (continuation lines start with `»`). Never rename, reorder or rewrite logic; keep original comments, or cut them whole.
- Annotations are margin badges explained under the listing, never comments added to the code.
- **Cross-references are never typed by hand.** Chapters, sections, tables and figures are referenced by label (`@ch-keydir`); chapters not written yet use `#xref(<ch-…>)` with an entry in `planned` in `template/isss.typ`. A reference already prints "Chapter 2": don't write "Chapter @ch-x".
- **Apostrophes after digits.** Typst turns `'` after a digit (or after a reference that ends in one) into a prime. Write `’s` (U+2019) there, or rephrase; never put `'s` straight after a reference.
