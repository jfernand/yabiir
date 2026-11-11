#import "template/isss.typ": *

#show: book.with(
  title: "Append Only",
  display-title: [Append\ Only],
  subtitle: [Easing into databases one step at a time — from a single log file and a hash map to sorted keys, tables and indexes, built in Rust on yabiir.],
  author: "Javier",
  edition: "Incipient draft · 2026",
  band: "Logs · Indexes · Tables · Rust",
)

#set page(numbering: "i")
#counter(page).update(1)
#bookmark[Contents]
#toc()

#set page(numbering: "1")
#counter(page).update(1)

// ── Part I — Finding Things ─────────────────────────────────────────────
#part("Finding Things", label: <part-finding>, blurb: [What `SELECT *` needs from a store, what an index is, what a hash index can and cannot answer, what keeping keys in order buys, and why a secondary index is one more table.])
#include "chapters/00-select-star.typ"
#include "chapters/01-the-keydir.typ"
#include "chapters/02-sorted-indexes.typ"
#include "chapters/03-secondary-indexes.typ"

#appendices[
#include "chapters/A-bags-and-nulls.typ"
#include "chapters/B-algebra-and-calculus.typ"
]

#group-start[Bibliography]
#show bibliography: set heading(bookmarked: false)
#bibliography("refs.bib", title: "Bibliography", style: "ieee")
