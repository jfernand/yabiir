// ═══════════════════════════════════════════════════════════════════════════
//  ISSS — light ("paper") book template for Typst
//  Copied from the JVM internals book (java-internals/template/isss.typ) and adapted
//  for the databases book: JDK badges and HotSpot listings replaced by yabiir ones.
//  Ported from hugo-theme-isss/assets/css/tokens.css
//  Industrial, utilitarian: condensed display type, safety-amber, steel greys.
// ═══════════════════════════════════════════════════════════════════════════

#import "@preview/cetz:0.4.2"

// ─── Tokens ────────────────────────────────────────────────────────────────
#let c = (
  black:     rgb("#0E0E0E"),
  ink:       rgb("#1A1A1A"),
  steel-900: rgb("#242424"),
  steel-800: rgb("#2E2E2E"),
  steel-700: rgb("#3A3A3A"),
  steel-600: rgb("#4A4A4A"),
  steel-500: rgb("#666666"),
  steel-400: rgb("#888888"),
  steel-300: rgb("#AAAAAA"),
  steel-200: rgb("#CCCCCC"),
  steel-100: rgb("#E5E5E5"),
  paper:     rgb("#F0EBE1"),
  paper-dk:  rgb("#E6DFD1"),   // sunken paper (inline code, table stripes)
  paper-dk2: rgb("#D9D0BE"),   // borders on paper
  white:     rgb("#FAFAF8"),
  amber-600: rgb("#C98A00"),
  amber-500: rgb("#E5A000"),
  amber-400: rgb("#F5B800"),
  amber-300: rgb("#FFCC33"),
  amber-200: rgb("#FFE280"),
  amber-100: rgb("#FFF4CC"),
  amber-txt: rgb("#8A5E00"),   // amber dark enough for text on paper (AA)
  red:       rgb("#CC2200"),
  red-light: rgb("#FF3311"),
  green:     rgb("#1A7A3C"),
  green-light: rgb("#22A850"),
  blue:      rgb("#0A4A7A"),
  blue-light: rgb("#1472C0"),
)

#let font-display = ("Barlow",)
#let font-body    = ("Space Grotesk", "Hanken Grotesk")
#let font-mono    = ("IBM Plex Mono", "DejaVu Sans Mono")

// Modular scale (1.25) anchored at 10pt body
#let ts = (xs: 6.4pt, sm: 8pt, base: 10pt, md: 12.5pt, lg: 15.6pt,
           xl: 19.5pt, x2: 24.4pt, x3: 30.5pt, x4: 47.7pt, x5: 74.5pt)

// Display text helper: condensed, uppercase, tracked
#let display(body, size: ts.md, weight: 700, fill: c.ink, tracking: 0.02em) = text(
  font: font-display, stretch: 75%, weight: weight, size: size,
  fill: fill, tracking: tracking, hyphenate: false, upper(body),
)

// Mono "kicker" / label helper
#let kicker(body, fill: c.amber-txt, size: ts.sm) = text(
  font: font-mono, size: size, weight: 500, fill: fill,
  tracking: 0.15em, upper(body),
)

// ─── Inline components ─────────────────────────────────────────────────────

// Version tag: #since("0.6")  →  [yabiir 0.6+]
#let since(v, suffix: "+") = box(baseline: 15%,
  box(stroke: 0.75pt + c.amber-600, fill: c.amber-100, inset: (x: 3pt, y: 1.5pt), outset: (y: 0.5pt),
    text(font: font-mono, size: 0.72em, weight: 600, fill: c.amber-txt)[yabiir #v#suffix]))

// ─── Spread-aware placement for code ──────────────────────────────────────
// Code must never continue across a page turn. A block that fits where it is stays
// there. Otherwise, on an even (left) page it may break, because it continues on the
// facing odd (right) page; on an odd page it moves, whole, to the next (left) page.
// A block taller than a page starts on a left page.
//   make: breakable (bool) => the block to place
#let page-h = 254mm
#let page-margin-top = 22mm
#let page-margin-bottom = 20mm
#let text-width = 138mm
// Invisible page markers inside each code block, so a layout check can query them:
//   typst query databases.typ "<code-start>" --field value
#let _code-mark(kind) = context [#metadata(here().page()) #label(kind)]

// The position is read from an anchor placed just before the context, not from the
// context itself: a page break inside the context would otherwise move its own
// reference point onto the next page, and the decision would flip on every layout pass.
#let spread(make) = {
  [#metadata(none) <spread-anchor>]
  context {
  let at = query(selector(<spread-anchor>).before(here())).last().location().position()
  let full = page-h - page-margin-top - page-margin-bottom
  let h = measure(make(false), width: text-width).height
  let remain = page-h - page-margin-bottom - at.y
  let left-page = calc.even(at.page)
  if h <= remain { make(false) }                                   // fits here
  else if left-page and remain >= 3cm { make(true) }                // runs onto the facing page
  else if h <= full { make(false) }                                 // moves whole to the next page
  else { if not left-page { pagebreak(weak: true) }; make(true) }   // taller than a page: start on a left page
  }
}

// ─── Block components ──────────────────────────────────────────────────────

#let _callout(label, accent, bg, body, title: none, breakable: false) = block(
  width: 100%, breakable: breakable, above: 1.2em, below: 1.2em,
  stroke: (left: 3pt + accent), fill: bg, inset: (left: 12pt, right: 10pt, y: 9pt),
)[
  #kicker(label, fill: accent.darken(15%), size: ts.xs + 0.6pt)
  #if title != none [ #h(4pt) #text(font: font-display, stretch: 75%, weight: 700,
      size: ts.base + 1pt, upper(title)) ]
  #v(-2pt)
  #set text(size: 0.95em)
  #body
]

#let note(body, title: none, breakable: false) = _callout("Note", c.steel-600, c.paper-dk, body, title: title, breakable: breakable)
#let warn(body, title: none)    = _callout("Warning",   c.red,       rgb("#F3DED4"), body, title: title)
#let deep(body, title: none)    = _callout("Under the hood", c.amber-600, c.amber-100, body, title: title)
#let changed(v, body, title: none) = _callout("Changed in yabiir " + str(v), c.blue, rgb("#DCE4EA"), body, title: title)

// Editorial marker: highlights a passage flagged for revision (remove before publishing)
#let flagged(body) = highlight(fill: rgb("#F6B9A8"), extent: 1pt, text(fill: c.ink, body))

// Draft status under a chapter opener: what state the chapter is in
#let status(body) = block(width: 100%, above: 0pt, below: 1.6em, breakable: false,
  stroke: (y: 0.5pt + c.paper-dk2), inset: (y: 6pt),
  grid(columns: (auto, 1fr), column-gutter: 10pt, align: (left + horizon, left + horizon),
    box(fill: c.amber-400, inset: (x: 4pt, y: 2pt),
      text(font: font-mono, size: ts.xs + 0.6pt, weight: 700, fill: c.ink, tracking: 0.15em, "DRAFT")),
    text(size: ts.sm + 1pt, fill: c.steel-600, body)))

// Still to write (end of an incipient chapter). Remove as the chapter fills in.
#let todo(..items) = block(width: 100%, above: 1.6em, below: 1.2em, breakable: false,
  stroke: (paint: c.steel-400, thickness: 0.75pt, dash: "dashed"), inset: 12pt)[
  #kicker("Still to write", fill: c.steel-600)
  #v(2pt)
  #set text(size: 0.95em, fill: c.steel-700)
  #set list(marker: text(fill: c.steel-400)[□], body-indent: 7pt)
  #for i in items.pos() [ - #i ]
]

// Key-takeaways box (end of chapter)
#let takeaways(..items) = block(
  width: 100%, above: 1.6em, below: 1.2em, breakable: false,
  fill: c.ink, inset: 14pt,
)[
  #set text(fill: c.paper, size: 0.95em)
  #show raw.where(block: false): it => box(fill: c.steel-800, inset: (x: 2.5pt), outset: (y: 2.5pt),
    text(font: font-mono, size: 7.8pt * 1.08, fill: c.amber-200, it.text))
  #kicker("Key takeaways", fill: c.amber-400)
  #v(2pt)
  #set list(marker: text(fill: c.amber-400)[■], body-indent: 7pt)
  #for i in items.pos() [ - #i ]
]

// Terminal / log output block (light variant of code)
#let output(body, title: "output", size: 7.2pt, breakable: false) = block(
  width: 100%, above: 1em, below: 1em, breakable: breakable,
  stroke: 0.75pt + c.paper-dk2, fill: c.white, inset: 0pt,
)[
  #block(width: 100%, fill: c.paper-dk, inset: (x: 8pt, y: 3pt), below: 0pt,
    kicker(title, fill: c.steel-600, size: ts.xs + 0.4pt))
  #show raw.where(block: true): r => block(width: 100%, text(font: font-mono, size: size, fill: c.steel-800, r.text))
  #set par(justify: false, leading: 0.5em)
  #block(inset: (x: 8pt, y: 7pt), above: 0pt,
    text(font: font-mono, size: size, fill: c.steel-800, body))
]

// Epigraph: a short quotation under a chapter's opening, with its source.
#let epigraph(body, source: none) = align(right, block(width: 62%, above: 0.4em, below: 1.8em,
  breakable: false, align(left)[
    #set par(justify: false)
    #text(size: ts.md, fill: c.steel-700, emph(body))
    #if source != none { v(-0.3em); align(right, text(size: ts.sm, fill: c.amber-txt, [— ] + source)) }
  ]))

// ─── Syntax (railroad) diagrams ─────────────────────────────────────────────
// `#syntax(title: …, rr-seq(rr-kw("SELECT"), rr-opt(rr-kw("DISTINCT", new: true)), …))`.
// Keywords and punctuation are rounded boxes (`rr-kw`), placeholders are square
// boxes (`rr-ph`). `new: true` paints an item amber: the grammar the section adds.
// Read left to right along the line; a branch is a choice, a bypass makes an item
// optional, a loop under an item repeats it, passing through its separator.
#let rr-kw(s, new: false) = (kind: "box", label: s, round: true, new: new)
#let rr-ph(s, new: false) = (kind: "box", label: s, round: false, new: new)
#let rr-skip = (kind: "skip")
#let rr-seq(..items) = (kind: "seq", items: items.pos())
#let rr-choice(..items) = (kind: "choice", items: items.pos())
#let rr-opt(item) = rr-choice(rr-skip, item)
#let rr-loop(item, sep) = (kind: "loop", item: item, sep: sep)

#let _rr-r = 6pt          // radius of every bend
#let _rr-gap = 8pt        // line between items in a sequence
#let _rr-vgap = 7pt       // space between stacked branches
#let _rr-h = 15pt         // height of a box
#let _rr-stroke = 0.9pt + c.steel-700
#let _rr-line(a, b) = place(top + left, line(start: a, end: b, stroke: _rr-stroke))
// a quarter bend from a to b, turning at the corner `k`
#let _rr-bend(a, k, b) = place(top + left,
  curve(stroke: _rr-stroke, curve.move(a), curve.quad(k, b)))

// Lay a node out. The line enters at (0, 0) and leaves at (w, 0); the node
// reaches `up` above the line and `down` below it. `draw(x, y)` returns the
// placed pieces with the line's entry at (x, y).
#let _rr-layout(n) = {
  let r = _rr-r
  if n.kind == "skip" {
    (w: 0pt, up: 0pt, down: 0pt, draw: (x, y) => ())
  } else if n.kind == "box" {
    let label = if n.round { text(font: font-mono, size: 8pt, weight: 700, fill: c.ink, n.label) }
                else { text(font: "Hanken Grotesk", size: 8.4pt, style: "italic", fill: c.ink, n.label) }
    let w = measure(label).width + if n.round { 14pt } else { 12pt }
    let h = _rr-h
    let (fill, stroke) = if n.new { (c.amber-200, 1pt + c.amber-600) }
                         else if n.round { (c.white, _rr-stroke) } else { (c.paper-dk, _rr-stroke) }
    (w: w, up: h / 2, down: h / 2, draw: (x, y) => (
      place(top + left, dx: x, dy: y - h / 2,
        box(width: w, height: h, radius: if n.round { h / 2 } else { 1.5pt },
          fill: fill, stroke: stroke, align(center + horizon, label))),
    ))
  } else if n.kind == "seq" {
    let parts = n.items.map(_rr-layout)
    let w = parts.map(p => p.w).sum(default: 0pt) + _rr-gap * calc.max(parts.len() - 1, 0)
    (w: w, up: calc.max(0pt, ..parts.map(p => p.up)), down: calc.max(0pt, ..parts.map(p => p.down)),
     draw: (x, y) => {
       let out = ()
       let cx = x
       for (i, p) in parts.enumerate() {
         if i > 0 { out.push(_rr-line((cx, y), (cx + _rr-gap, y))); cx += _rr-gap }
         out += (p.draw)(cx, y)
         cx += p.w
       }
       out
     })
  } else if n.kind == "choice" {
    // the first branch is the main line; the others hang below it
    let parts = n.items.map(_rr-layout)
    let inner = calc.max(..parts.map(p => p.w))
    let w = inner + 4 * r
    let ys = (0pt,)
    for i in range(1, parts.len()) {
      ys.push(ys.at(i - 1) + parts.at(i - 1).down + _rr-vgap + calc.max(parts.at(i).up, r))
    }
    let down = calc.max(parts.at(0).down, ys.last() + parts.last().down)
    (w: w, up: parts.at(0).up, down: down, draw: (x, y) => {
      let out = ()
      for (i, p) in parts.enumerate() {
        let by = y + ys.at(i)
        let px = x + 2 * r + (inner - p.w) / 2
        if i == 0 {
          out.push(_rr-line((x, y), (px, y)))
          out.push(_rr-line((px + p.w, y), (x + w, y)))
        } else {
          out.push(_rr-bend((x, y), (x + r, y), (x + r, y + r)))
          out.push(_rr-line((x + r, y + r), (x + r, by - r)))
          out.push(_rr-bend((x + r, by - r), (x + r, by), (x + 2 * r, by)))
          out.push(_rr-line((x + 2 * r, by), (px, by)))
          out.push(_rr-line((px + p.w, by), (x + w - 2 * r, by)))
          out.push(_rr-bend((x + w - 2 * r, by), (x + w - r, by), (x + w - r, by - r)))
          out.push(_rr-line((x + w - r, by - r), (x + w - r, y + r)))
          out.push(_rr-bend((x + w - r, y + r), (x + w - r, y), (x + w, y)))
        }
        out += (p.draw)(px, by)
      }
      out
    })
  } else if n.kind == "loop" {
    // the item on the main line, the way back underneath through the separator
    let item = _rr-layout(n.item)
    let sep = _rr-layout(n.sep)
    let inner = calc.max(item.w, sep.w)
    let w = inner + 4 * r
    let by = item.down + _rr-vgap + calc.max(sep.up, r)
    (w: w, up: item.up, down: by + sep.down, draw: (x, y) => {
      let ix = x + 2 * r + (inner - item.w) / 2
      let sx = x + 2 * r + (inner - sep.w) / 2
      let b = y + by
      (
        _rr-line((x, y), (ix, y)),
        _rr-line((ix + item.w, y), (x + w, y)),
        _rr-bend((x + w - 2 * r, y), (x + w - r, y), (x + w - r, y + r)),
        _rr-line((x + w - r, y + r), (x + w - r, b - r)),
        _rr-bend((x + w - r, b - r), (x + w - r, b), (x + w - 2 * r, b)),
        _rr-line((x + w - 2 * r, b), (sx + sep.w, b)),
        _rr-line((sx, b), (x + 2 * r, b)),
        _rr-bend((x + 2 * r, b), (x + r, b), (x + r, b - r)),
        _rr-line((x + r, b - r), (x + r, y + r)),
        _rr-bend((x + r, y + r), (x + r, y), (x + 2 * r, y)),
      ) + (item.draw)(ix, y) + (sep.draw)(sx, b)
    })
  }
}

#let syntax(diagram, title: "Syntax so far") = block(width: 100%, above: 1.1em, below: 1.1em,
  breakable: false, stroke: 0.75pt + c.paper-dk2, fill: c.white, inset: 0pt)[
  #block(width: 100%, fill: c.paper-dk, inset: (x: 8pt, y: 3pt), below: 0pt,
    kicker(title, fill: c.steel-600, size: ts.xs + 0.4pt))
  #block(inset: (x: 10pt, y: 9pt), above: 0pt, width: 100%, align(center, layout(size => {
    let n = _rr-layout(diagram)
    let lead = 12pt
    let pad = 3pt
    let w = n.w + 2 * lead + 2 * pad
    let y = n.up + pad
    let x0 = pad
    let x1 = pad + lead + n.w + lead
    // a grammar wider than the page is drawn smaller, never cut
    let k = calc.min(1, size.width / w)
    scale(k * 100%, reflow: true, box(width: w, height: n.up + n.down + 2 * pad, {
      // start: two bars; end: one bar
      _rr-line((x0, y - 5pt), (x0, y + 5pt))
      _rr-line((x0 + 2.5pt, y - 5pt), (x0 + 2.5pt, y + 5pt))
      _rr-line((x0 + 2.5pt, y), (x0 + lead, y))
      for piece in (n.draw)(x0 + lead, y) { piece }
      _rr-line((x0 + lead + n.w, y), (x1, y))
      _rr-line((x1, y - 5pt), (x1, y + 5pt))
    }))
  })))
]

// ─── Appendices ────────────────────────────────────────────────────────────
// `#show: appendices` before the first appendix: letters instead of numbers.
#let in-appendix = state("isss-appendix", false)
#let in-source = state("isss-source", false)
// mode at a location: "main" | "source" | "appendix"
#let mode-at(loc) = if in-appendix.at(loc) { "appendix" } else if in-source.at(loc) { "source" } else { "main" }
// The PDF outline nests chapters under their part. Typst nests bookmarks by heading level, so
// every heading of the book is set one level down (`offset: 1` in `book`), and each part, and
// the appendices and bibliography, gets a hidden level-1 heading that only the outline shows.
// The heading counter then has a leading 0 for that level, which `hnum` and `chap-of` skip.
// The number the first chapter gets. This book starts at Chapter 0; counters still start at 1,
// so every main-matter chapter number is shifted by `first-chapter - 1`.
#let first-chapter = 0
#let shift-chapter(n) = n - 1 + first-chapter
#let hnum(pattern) = (..n) => {
  let ns = n.pos().slice(1)
  if pattern.starts-with("1") and ns.len() > 0 { ns.at(0) = shift-chapter(ns.at(0)) }
  numbering(pattern, ..ns)
}
#let chap-of(nums) = nums.at(1, default: 0)
#let bookmark(title) = heading(depth: 1, offset: 0, outlined: false, bookmarked: true, numbering: none, title)
// A top-level outline entry for a group that has no divider page (the appendices, the
// bibliography). It goes on the right-hand page where the group starts, and the chapter opener
// that follows skips its own page break: the entry would otherwise leave a page to itself.
#let broken = state("isss-broken", false)
#let group-start(title) = { pagebreak(weak: true, to: "odd"); bookmark(title); broken.update(true) }

// Two digits for chapters 1-9 ("01"); numbering("01", n) would give "010" from 10 on.
#let chapter-num(m, n) = if m == "appendix" { numbering("A", n) } else if m == "source" { numbering("S.1", n) } else { let k = shift-chapter(n); if k < 10 { "0" + str(k) } else { str(k) } }
#let appendices(body) = {
  group-start[Appendices]
  counter(heading).update(0)
  in-source.update(false)
  in-appendix.update(true)
  set heading(numbering: hnum("A.1"), supplement: [Section])
  show heading.where(level: 2): set heading(supplement: [Appendix])
  body
}

// ─── Source walkthroughs ("Reading the Source") ────────────────────────────
// `#show: source-part` before the first walkthrough. Each level-1 heading is a
// walkthrough numbered S.1, S.2…; it opens on a fresh page with a compact opener.
#let source-part(body) = {
  counter(heading).update(0)
  in-source.update(true)
  // "S.1" as a pattern would repeat its prefix at every level ("S.16S.1")
  set heading(numbering: (..n) => "S." + n.pos().slice(1).map(str).join("."), supplement: [Walkthrough])
  body
}

// A review build renders one source file on its own (review/chunk.typ, --input file=…);
// references into other files then show the label instead of failing.
#let review-mode = "file" in sys.inputs
#let review-placeholder(target) = text(fill: c.amber-txt, weight: 600, "→" + str(target))

// Short cross-reference marker from a chapter to a walkthrough: → S.1
#let _srcref(target) = link(target, context {
  let h = query(target).first()
  box(text(font: font-mono, size: 0.8em, weight: 600, fill: c.amber-txt,
    "→ " + numbering(h.numbering, ..counter(heading).at(h.location()))))
})
#let srcref(target) = if review-mode { context if query(target).len() == 0 { review-placeholder(target) } else { _srcref(target) } } else { _srcref(target) }

// The sections a walkthrough serves, under its opener
#let serves(..targets) = block(above: 0pt, below: 1.4em,
  grid(columns: (auto, 1fr), column-gutter: 8pt, align: horizon,
    kicker("Serves", size: ts.xs + 0.6pt, fill: c.steel-500),
    text(size: ts.sm + 0.5pt, targets.pos().map(t => ref(t)).join([ · ]))))

// Callout badge used in source listings and their notes
#let _badge(n) = box(fill: c.amber-400, inset: (x: 2.6pt, y: 0pt), outset: (y: 1.2pt), radius: 1.5pt,
  text(font: font-mono, size: 6.2pt, weight: 700, fill: c.ink, str(n)))

// A pinned, line-numbered yabiir excerpt.
//   path:  file path under `root`, e.g. "keydir.rs"
//   lines: ((from, to), ...) original line ranges, in order; each range maps onto the
//          listing's non-elided lines. A line that is exactly `// …` marks an elision;
//          a line starting with "»" is a wrapped continuation (no number of its own).
//   notes: ((line, [text]), ...) margin callouts keyed by original line number.
// Pinned to a published release tag. Up to v0.5.0 the crate sources are under src/; from the
// workspace split on, they are under crates/yabiir/src/ (pass `root:` for a later tag).
#let src-base = "https://github.com/jfernand/yabiir/blob/"
#let srcfile(path, code, lines: (), notes: (), tag: "v0.5.0", root: "src/", size: 7.2pt) = {
  let nums = ()
  for r in lines { for i in range(r.first(), r.last() + 1) { nums.push(i) } }
  // A listing line starting with "»" continues the previous source line (wrapped to fit).
  let labels = ()
  let clean = ()
  let k = 0
  for l in code.text.split("\n") {
    if l.trim() == "// …" { labels.push(none); clean.push(l) }
    else if l.starts-with("»") { labels.push(none); clean.push(l.trim("»", at: start)) }
    else { labels.push(nums.at(k, default: none)); clean.push(l); k += 1 }
  }
  let listing = raw(clean.join("\n"), block: true, lang: code.lang)
  assert(k == nums.len(), message: path + ": listing has " + str(k) + " lines, ranges cover " + str(nums.len()))
  let marks = (:)
  for (i, nt) in notes.enumerate() {
    assert(nt.first() in nums, message: path + ": note on line " + str(nt.first()) + " outside the listing")
    marks.insert(str(nt.first()), i + 1)
  }
  let first = lines.first().first()
  let last = lines.last().last()
  let url = src-base + tag + "/" + root + path + "#L" + str(first) + "-L" + str(last)
  spread(brk => block(breakable: brk, above: 1.2em, below: 1.2em, width: 100%)[
    #_code-mark("code-start")
    #block(width: 100%, below: 0pt, fill: c.black, stroke: (left: 3pt + c.amber-400),
      inset: (left: 11pt, right: 9pt, y: 5pt),
      grid(columns: (1fr, auto), align: (left, right),
        link(url, text(font: font-mono, size: 6.6pt, fill: c.amber-300, path)),
        text(font: font-mono, size: 6.6pt, fill: c.steel-400,
          tag + " · L" + str(first) + "–" + str(last))))
    #{
      // own dark block (instead of the global raw style) so the file bar sits flush on top
      show raw.where(block: true): it => block(width: 100%, above: 0pt, breakable: true,
        fill: c.ink, stroke: (left: 3pt + c.amber-400), inset: (left: 11pt, right: 9pt, top: 7pt, bottom: 9pt))[
        #set par(justify: false, leading: 0.55em)
        #set text(font: font-mono, size: size, fill: c.paper)
        #it.lines.join(linebreak())
      ]
      show raw.line: it => {
        let lab = labels.at(it.number - 1, default: none)
        box(width: 2.4em, align(right, text(fill: c.steel-500, if lab != none { str(lab) })))
        h(1em)
        it.body
        if lab != none and str(lab) in marks { h(1fr); _badge(marks.at(str(lab))) }
      }
      listing
    }
    #_code-mark("code-end")
  ])
  // The notes are prose: they may follow the listing onto any page.
  if notes.len() > 0 {
    set text(size: ts.sm + 1pt)
    set par(justify: false, leading: 0.6em)
    block(above: -0.4em, below: 1.2em, grid(columns: (auto, 1fr), column-gutter: 7pt, row-gutter: 0.75em,
      ..notes.enumerate().map(((i, nt)) => (_badge(i + 1), nt.last())).flatten()))
  }
}

// ─── The book's repository ────────────────────────────────────────────────
#let repo-url = "https://github.com/jfernand/yabiir"
// A linked path inside the repository, e.g. #repo("code/ch02/Lazy.java")
#let repo(path) = link(repo-url + "/tree/master/" + path, raw(path))

// ─── Landmark: a linked place of interest (Appendix "Places of Interest") ──
//   #landmark("Name", "https://…")[Why it's worth a visit.]
#let landmark(name, url, body) = block(above: 0.9em, below: 0.9em, breakable: false,
  stroke: (left: 1.5pt + c.paper-dk2), inset: (left: 9pt, y: 1pt))[
  #text(weight: 700, name)
  #h(6pt)
  #box(link(url, text(font: font-mono, size: ts.xs + 1pt, fill: c.amber-txt,
    url.trim("https://", at: start).trim("/", at: end))))
  #v(-4pt)
  #set par(justify: false)
  #text(size: ts.sm + 1.5pt, fill: c.steel-700, body)
]

// ─── Cross-references, including to chapters not yet written ──────────────
// Planned chapters and parts, by label. A reference to one prints the planned name
// until a heading (or part) with that label exists; then it becomes a real reference.
// Remove an entry once its chapter is written.
#let planned = (
)
#let xref(target) = context {
  let found = query(target)
  if found.len() > 0 {
    let el = found.first()
    if el.func() == metadata { link(target, text(fill: c.amber-txt, weight: 600, "Part " + numbering("I", el.value.n))) }
    else { ref(target) }
  } else {
    if review-mode and str(target) not in planned { return review-placeholder(target) }
    assert(str(target) in planned, message: "xref: no label or planned entry for " + str(target))
    planned.at(str(target))
  }
}

// ─── Part divider (full dark page) ─────────────────────────────────────────
#let part-counter = counter("isss-part")

#let part(title, blurb: none, label: none) = {
  pagebreak(weak: true, to: "odd")
  part-counter.step()
  page(fill: c.black, header: none, footer: none, margin: (x: 2.2cm, y: 2.6cm))[
    #context {
      let n = part-counter.get().first()
      [#metadata((title: title, n: n)) <isss-part>]
      bookmark("Part " + numbering("I", n) + " · " + title)
      if label != none { [#metadata((title: title, n: n)) #label] }
      v(1fr)
      kicker("Part " + numbering("I", n), fill: c.amber-400, size: ts.base)
      v(6pt)
      line(length: 3cm, stroke: 4pt + c.amber-400)
      v(10pt)
      par(leading: 0.15em, display(title, size: ts.x4, weight: 800, fill: c.paper, tracking: -0.01em))
      if blurb != none {
        v(14pt)
        block(width: 80%, text(font: font-body, size: ts.md, fill: c.steel-300, blurb))
      }
      v(2fr)
      kicker("Industrial Strength Software Services", fill: c.steel-500, size: ts.xs + 0.6pt)
    }
  ]
}

// ─── Table of contents (parts + chapters + sections) ───────────────────────
#let toc() = {
  display("Contents", size: ts.x3, weight: 800)
  v(-4pt)
  line(length: 100%, stroke: 3pt + c.amber-400)
  v(10pt)
  context {
    let items = query(selector(<isss-part>).or(heading.where(level: 2)).or(heading.where(level: 3)))
    for it in items {
      let loc = it.location()
      let pg = counter(page).at(loc).first()
      if it.func() == metadata {
        v(18pt)
        block(above: 0pt, below: 10pt, link(loc)[
          #kicker("Part " + numbering("I", it.value.n), size: ts.sm)
          #h(6pt)
          #display(it.value.title, size: ts.md, weight: 800)
        ])
      } else if it.level == 2 {
        let m = mode-at(loc)
        let num = if it.numbering != none { chapter-num(m, chap-of(counter(heading).at(loc))) } else { none }
        block(above: if m == "source" { 8pt } else { 14pt }, below: 7pt, link(loc, grid(
          columns: (2.6em, 1fr, auto), align: (left + bottom, left + bottom, right + bottom),
          text(font: font-mono, size: ts.sm, weight: 600, fill: c.amber-txt)[#if num != none { num }],
          [#display(it.body, size: ts.base + 2pt, weight: 700) #box(width: 1fr, repeat(text(fill: c.steel-300, size: ts.sm)[.#h(2pt)]))],
          text(font: font-mono, size: ts.sm, weight: 600)[#pg],
        )))
      } else {
        let nums = counter(heading).at(loc).slice(1)
        block(above: 5.5pt, below: 5.5pt, link(loc, grid(
          columns: (2.2em, 2.6em, 1fr, auto), align: (left, left, left, right),
          [], text(font: font-mono, size: ts.xs + 1pt, fill: c.steel-500, if in-appendix.at(loc) { numbering("A.1", ..nums) } else { numbering("1.1", shift-chapter(nums.first()), ..nums.slice(1)) }),
          text(size: ts.sm + 0.5pt, fill: c.steel-700, it.body),
          text(font: font-mono, size: ts.xs + 1pt, fill: c.steel-500)[#pg],
        )))
      }
    }
  }
}

// ─── Title page ────────────────────────────────────────────────────────────
#let title-page(title: none, display-title: none, subtitle: none, author: none, edition: none, band: none) = page(
  fill: c.black, header: none, footer: none, margin: (x: 2.2cm, top: 2.4cm, bottom: 2cm),
)[
  #set text(fill: c.paper)
  #grid(columns: (1fr, auto),
    kicker("ISSS // Technical Series", fill: c.amber-400, size: ts.sm),
    kicker(edition, fill: c.steel-400, size: ts.sm))
  #v(4pt)
  #line(length: 100%, stroke: 0.75pt + c.steel-700)
  #v(1fr)
  #if band != none { block(width: 100%, fill: c.amber-400, inset: (x: 10pt, y: 6pt),
    kicker(band, fill: c.black, size: ts.sm)) }
  #v(14pt)
  #par(leading: 0.1em, display(if display-title != none { display-title } else { title }, size: ts.x5 + 10pt, weight: 800, fill: c.paper, tracking: -0.02em))
  #v(18pt)
  #block(width: 85%, text(size: ts.lg, fill: c.steel-300, subtitle))
  #v(2fr)
  #line(length: 100%, stroke: 0.75pt + c.steel-700)
  #v(4pt)
  #grid(columns: (1fr, auto),
    kicker(author, fill: c.paper, size: ts.sm),
    kicker("Technology before enshittification.", fill: c.steel-500, size: ts.sm))
]

// ─── Main template ─────────────────────────────────────────────────────────
#let book(title: "", display-title: none, subtitle: none, author: none, edition: none, band: none, cover: true, body) = {
  set document(title: title, author: if author != none { author } else { () })

  set page(
    width: 178mm, height: 254mm,  // ~7 × 10 in technical book trim
    fill: c.paper,
    margin: (inside: 22mm, outside: 18mm, top: 22mm, bottom: 20mm),
    header: context {
      let here-pg = here().page()
      // suppress on chapter-opening pages
      let opens = query(heading.where(level: 2)).filter(h => h.location().page() == here-pg)
      if opens.len() > 0 and mode-at(opens.first().location()) != "source" { return }
      let prev = query(heading.where(level: 2).before(here())) + opens
      let chap = if prev.len() > 0 { prev.last() } else { none }
      if chap == none { return }
      let num = if chap.numbering != none {
        let m = mode-at(chap.location())
        (if m == "appendix" { "Appendix " } else if m == "source" { "Walkthrough " } else { "" }) + chapter-num(m, chap-of(counter(heading).at(chap.location()))) + " // "
      } else { "" }
      set text(font: font-mono, size: ts.xs + 0.6pt, fill: c.steel-500, tracking: 0.12em)
      grid(columns: (1fr, auto),
        upper(num + chap.body), upper(title))
      v(-6pt)
      line(length: 100%, stroke: 0.5pt + c.paper-dk2)
    },
    footer: context {
      let pg = counter(page).display("1")
      let is-odd = calc.odd(here().page())
      let num = box(fill: c.ink, inset: (x: 5pt, y: 2.5pt),
        text(font: font-mono, size: ts.sm, weight: 600, fill: c.amber-400, pg))
      if is-odd { align(right, num) } else { align(left, num) }
    },
  )

  set text(font: font-body, size: ts.base, fill: c.ink, lang: "en", hyphenate: true,
           number-type: "lining")
  set par(justify: true, leading: 0.72em, spacing: 1.05em, first-line-indent: 0pt)
  set list(marker: (text(fill: c.amber-600)[■], text(fill: c.steel-500)[–]), indent: 4pt, body-indent: 6pt)
  set enum(indent: 4pt, body-indent: 6pt,
    numbering: n => text(font: font-mono, weight: 600, fill: c.amber-txt, size: 0.9em, str(n) + "."))
  set terms(separator: [ — ], hanging-indent: 1.2em)
  show strong: set text(weight: 700)
  // Space Grotesk ships no italic, so Typst would set _emphasis_ upright and it would vanish.
  // Hanken Grotesk, already the body fallback, has one; emphasis uses it.
  show emph: set text(font: ("Hanken Grotesk",))
  show link: it => {
    if type(it.dest) == str { text(fill: c.blue, it) } else { it }
  }
  show ref: it => text(fill: c.amber-txt, weight: 600, it)
  set footnote.entry(separator: line(length: 25%, stroke: 0.5pt + c.steel-400))
  show footnote.entry: set text(size: ts.sm)

  // Headings ------------------------------------------------------------
  set heading(numbering: hnum("1.1"), offset: 1)
  show heading.where(level: 1): it => place(hide(box()))   // the hidden bookmarks
  show heading.where(level: 2): set heading(supplement: [Chapter])
  // Typst sets a heading's text at 1.4em for level 1, 1.2em for level 2 and 1em below that, and
  // the openers' spacing is in em: this keeps chapters and walkthroughs at the size of their
  // level before the offset (1.2em × 7/6 = 1.4em). Sections are now level 3, at 1em, so their
  // spacing below is scaled by 1.2 instead.
  show heading.where(level: 2): set text(size: 1.4em / 1.2)

  show heading.where(level: 2): it => {
    let m = mode-at(here())
    if m == "source" {
      // compact walkthrough opener, on a right-hand page like a chapter's: that fixes which
      // pages face each other inside it, so its listings' layout doesn't depend on the
      // walkthroughs before it
      pagebreak(weak: true, to: "odd")
      let num = if it.numbering != none { chapter-num(m, chap-of(counter(heading).get())) } else { none }
      block(below: 0pt, sticky: true)[
        #grid(columns: (auto, 1fr), column-gutter: 8pt, align: bottom,
          kicker(if num != none { "Walkthrough" } else { "Reading the Source" }, size: ts.sm),
          line(length: 100%, stroke: 0.5pt + c.steel-300))
        #v(4pt)
        #grid(columns: if num != none { (auto, 1fr) } else { (1fr,) }, column-gutter: 10pt, align: bottom,
          ..(if num != none { (text(font: font-display, stretch: 75%, weight: 800, size: ts.x3, fill: c.amber-400, num),) } else { () }),
          par(leading: 0.12em, justify: false, display(it.body, size: ts.xl + 2pt, weight: 800)))
        #v(2pt)
        #line(length: 100%, stroke: 3pt + c.amber-400)
        #v(8pt)
      ]
      return
    }
    if not broken.get() { pagebreak(weak: true, to: "odd") }
    broken.update(false)
    let app = m == "appendix"
    let num = if it.numbering != none { chapter-num(m, chap-of(counter(heading).get())) } else { none }
    v(2.4cm)
    block(below: 0pt)[
      #if num != none {
        grid(columns: (auto, 1fr), column-gutter: 8pt, align: bottom,
          kicker(if app { "Appendix" } else { "Chapter" }, size: ts.sm),
          line(length: 100%, stroke: 0.5pt + c.steel-300))
        v(-2pt)
        text(font: font-display, stretch: 75%, weight: 800, size: ts.x5, fill: c.amber-400, num)
        v(-34pt)
      }
      #par(leading: 0.12em, justify: false,
        display(it.body, size: ts.x3 + 4pt, weight: 800, tracking: -0.01em))
    ]
    v(10pt)
    line(length: 100%, stroke: 4pt + c.amber-400)
    v(22pt)
  }

  show heading.where(level: 3): it => block(above: 2.4em, below: 1.08em, sticky: true)[
    #if it.numbering != none {
      text(font: font-mono, size: ts.sm + 1pt, weight: 600, fill: c.amber-txt,
        counter(heading).display(it.numbering))
      h(8pt)
    }
    #display(it.body, size: ts.lg + 1pt, weight: 800)
    #v(-8pt)
    #line(length: 100%, stroke: 0.75pt + c.ink)
  ]

  show heading.where(level: 4): it => block(above: 1.5em, below: 0.7em, sticky: true)[
    #box(width: 6pt, height: 6pt, fill: c.amber-400, baseline: -1pt)
    #h(5pt)
    #display(it.body, size: ts.md, weight: 700)
  ]

  show heading.where(level: 5): it => block(above: 1.2em, below: 0.6em, sticky: true,
    kicker(it.body, fill: c.ink, size: ts.sm))

  // Code ----------------------------------------------------------------
  set raw(theme: "isss-dark.tmTheme")
  show raw: set text(font: font-mono)
  show raw.where(block: true): set text(size: 7.8pt)
  show raw.where(block: false): set text(size: 1.05em)   // Typst already scales raw to 0.8em; net ≈ 0.84em of surrounding text (body, footnotes, tables)
  show raw.where(block: true): it => spread(brk => block(
    width: 100%, above: 1em, below: 1.1em, breakable: brk,
    fill: c.ink, stroke: (left: 3pt + c.amber-400),
    inset: (left: 11pt, right: 9pt, y: 9pt),
  )[
    #if it.lang != none {
      place(top + right, dx: 4pt, dy: -5pt,
        text(font: font-mono, size: 6pt, fill: c.steel-500, tracking: 0.15em, upper(it.lang)))
    }
    #_code-mark("code-start")
    #set par(justify: false, leading: 0.55em)
    #set text(fill: c.paper)
    #it
    #_code-mark("code-end")
  ])
  show raw.where(block: false): it => box(
    fill: c.paper-dk, inset: (x: 2.5pt), outset: (y: 2.5pt),
    text(fill: c.ink, it),
  )

  // Tables --------------------------------------------------------------
  set table(
    stroke: (x, y) => (bottom: 0.5pt + c.paper-dk2, top: if y == 0 { 1.5pt + c.ink }),
    inset: (x: 6pt, y: 5pt),
    align: left,
    fill: (x, y) => if y == 0 { c.ink },
  )
  show table.cell.where(y: 0): set text(font: font-display, stretch: 75%, weight: 700,
    size: ts.base, fill: c.amber-400, tracking: 0.06em)
  show table.cell.where(y: 0): it => upper(it)
  show table: set text(size: ts.sm + 1pt)
  show table: set par(justify: false)
  // Tables never break across pages (STYLE.md §7), figure or not.
  show table: it => block(breakable: false, it)

  // Figures -------------------------------------------------------------
  set figure(gap: 8pt)
  show figure.caption: it => {
    set par(justify: false)
    set text(size: ts.sm + 0.5pt)
    kicker(it.supplement + " " + context it.counter.display(), size: ts.xs + 0.8pt)
    h(6pt)
    text(fill: c.steel-700, it.body)
  }
  show figure.where(kind: table): set figure.caption(position: top)
  show figure.where(kind: table): set block(breakable: false)
  set figure(numbering: n => {
    let ch = chap-of(counter(heading).get())
    let m = mode-at(here())
    if m == "appendix" { numbering("A.1", ch, n) } else if m == "source" { numbering("S.1.1", ch, n) } else { numbering("1.1", shift-chapter(ch), n) }
  })
  show heading.where(level: 2): it => {
    counter(figure.where(kind: image)).update(0)
    counter(figure.where(kind: table)).update(0)
    counter(figure.where(kind: raw)).update(0)
    it
  }

  // Math ----------------------------------------------------------------
  show math.equation: set text(font: ("New Computer Modern Math",))

  if cover { title-page(title: title, display-title: display-title, subtitle: subtitle, author: author, edition: edition, band: band) }
  body
}

// ─── Diagram helpers (cetz) ────────────────────────────────────────────────
// Re-exported so chapters can `#import "../template/isss.typ": *`
// Inside a diagram, inline code is set in the mono font without the shaded box it gets in
// running text: the boxes collide with lines and with each other at diagram sizes.
#let canvas(..args) = {
  show raw.where(block: false): it => text(font: font-mono, size: 0.92em, it.text)
  cetz.canvas(..args)
}
#let draw = cetz.draw

// A labelled memory "cell" row: cells = ((label, width, fill), ...)
#let memrow(cells, height: 0.8, origin: (0, 0), label-size: 6.5pt) = {
  import cetz.draw: *
  let x = origin.at(0)
  let y = origin.at(1)
  for (lbl, w, f) in cells {
    rect((x, y), (x + w, y + height), fill: f, stroke: 0.6pt + c.ink)
    content((x + w / 2, y + height / 2),
      text(font: font-mono, size: label-size, fill: if f == c.ink or f == c.steel-700 { c.paper } else { c.ink }, lbl))
    x += w
  }
}

// One recorded line of a probe, as each group of releases prints it.
#let recorded(title, ..rows) = output(title: title, size: 6.6pt,
  grid(columns: (auto, 1fr), row-gutter: 5pt, column-gutter: 10pt,
    ..rows.pos().map(((r, s)) => (text(weight: 600, fill: c.amber-txt, r), s)).flatten()))
