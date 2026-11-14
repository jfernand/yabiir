// Benchmark history report: Criterion medians for every release.
// Reads the bench-history/<version>.json files written by `cargo xtask campaign`.
// Build: typst compile bench-history/report.typ

#import "@preview/cetz:0.4.2": canvas, draw

#set document(title: "yabiir benchmark history")
#set page(paper: "a4", margin: (x: 2cm, y: 2cm), numbering: "1")
#set text(size: 10pt)
#set par(justify: true)
#set heading(numbering: "1.")
#show heading.where(level: 1): set block(above: 1.4em, below: 0.8em)
#show figure.caption: set text(size: 8.5pt)

// ---------------------------------------------------------------- data

#let versions = ("v0.2.0", "v0.3.0", "v0.4.0", "v0.5.0", "v0.5.1", "v0.6.0", "v0.7.0", "v0.8.0")
#let runs = versions.map(v => json(v + ".json"))
#let last = versions.len() - 1
// Indices of the releases the text talks about most.
#let v051 = 4
#let v06 = 5
#let v07 = 6
#let v08 = 7
#let campaign = runs.at(last).campaign
#let session = campaign.sessions.at(0)

#let med(i, k) = {
  let r = runs.at(i).results
  if k in r { r.at(k).median } else { none }
}

// The parameter after the last slash, as a number when it is one.
#let param(k) = {
  let p = k.split("/").last()
  if p.match(regex("^\d+$")) != none { int(p) } else { p }
}

#let keys-of(prefix) = {
  runs.at(last).results.keys().filter(k => k.starts-with(prefix + "/")).sorted(key: param)
}

// Geometric mean of the ratios of version i to version `base`, over `keys`.
#let gm(keys, i, base: 0) = calc.exp(
  keys.map(k => calc.ln(med(i, k) / med(base, k))).sum() / keys.len(),
)

// Benchmarks of code that did not change from v0.2.0 to v0.8.0: the CRC,
// entry encoding and header decoding. They measure the machine, not yabiir.
#let control-keys = (
  keys-of("verify_crc") + keys-of("encode_entry") + ("decode_entry_header", "decode_hint_header", "encode_hint")
)
#let control = range(versions.len()).map(i => gm(control-keys, i))

#let fmt-time(ns) = {
  let (v, u) = if ns < 1e3 { (ns, "ns") } else if ns < 1e6 { (ns / 1e3, "µs") } else if ns < 1e9 {
    (ns / 1e6, "ms")
  } else { (ns / 1e9, "s") }
  let d = if v < 10 { 2 } else if v < 100 { 1 } else { 0 }
  str(calc.round(v, digits: d)) + " " + u
}

#let fmt-pct(r) = {
  let p = calc.round((r - 1) * 100, digits: 1)
  if p > 0 { "+" + str(p) + "%" } else if p == 0 { "0%" } else { "−" + str(-p) + "%" }
}

// ---------------------------------------------------------------- charts

#let palette = (
  rgb("#1b6ca8"),
  rgb("#e07b00"),
  rgb("#2a9d5c"),
  rgb("#c0392b"),
  rgb("#7d4fb3"),
)
#let ref-color = luma(130)

#let swatch(c, dashed: false) = box(
  baseline: -0.15em,
  line(length: 10pt, stroke: (paint: c, thickness: 1.2pt, dash: if dashed { "dashed" } else { none })),
)

#let legend(items) = text(size: 7pt, items
  .map(((label, c, dashed)) => [#swatch(c, dashed: dashed)~#label])
  .join(h(0.8em)))

// A line chart of ratios against v0.2.0, one line per series, plus the
// control index dashed for reference.
#let ratio-panel(series, w: 7.0, h: 3.4, with-control: true, color: none) = {
  let all = series.map(s => s.at(1)).flatten().filter(x => x != none)
  if with-control { all += control }
  let lo = calc.min(0.97, ..all) - 0.02
  let hi = calc.max(1.08, ..all) + 0.02
  let span = hi - lo
  let step = if span <= 0.2 { 0.05 } else if span <= 0.5 { 0.1 } else { 0.25 }
  let ticks = range(int(calc.ceil(lo / step)), int(calc.floor(hi / step)) + 1).map(t => t * step)
  let n = versions.len()
  let X(i) = i * w / (n - 1)
  let Y(v) = (v - lo) / span * h

  canvas(length: 1cm, {
    import draw: *
    for t in ticks {
      let base = calc.abs(t - 1) < 1e-9
      line((0, Y(t)), (w, Y(t)), stroke: if base { 0.6pt + luma(120) } else { 0.3pt + luma(215) })
      content((-0.12, Y(t)), anchor: "east", text(size: 6pt, fill: luma(80), fmt-pct(t)))
    }
    for i in range(n) {
      content((X(i), -0.22), anchor: "north", text(size: 6pt, fill: luma(80), versions.at(i).slice(1)))
    }
    if with-control {
      line(
        ..range(n).map(i => (X(i), Y(control.at(i)))),
        stroke: (paint: ref-color, thickness: 0.9pt, dash: "dashed"),
      )
    }
    for (j, (label, values)) in series.enumerate() {
      let c = if color != none { color } else { palette.at(calc.rem(j, palette.len())) }
      let pts = range(n).filter(i => values.at(i) != none).map(i => (X(i), Y(values.at(i))))
      if pts.len() > 1 { line(..pts, stroke: 1.1pt + c) }
      for p in pts { circle(p, radius: 0.045, fill: c, stroke: none) }
    }
  })
}

// One panel per benchmark family: a line per parameter, relative to v0.2.0.
#let family-panel(prefix, title, unit: "") = {
  let keys = keys-of(prefix)
  let series = keys.map(k => (str(param(k)) + unit, range(versions.len()).map(i => med(i, k) / med(0, k))))
  block(breakable: false, width: 100%)[
    #text(size: 8.5pt, weight: "bold", title)
    #h(0.4em)
    #text(size: 7pt, fill: luma(90))[v0.2.0: #keys.map(k => fmt-time(med(0, k))).join(", ")]
    #v(-0.3em)
    #align(center, ratio-panel(series))
    #v(-0.2em)
    #align(center, legend(series.enumerate().map(((j, s)) => (
      s.at(0),
      palette.at(calc.rem(j, palette.len())),
      false,
    )) + (("control", ref-color, true),)))
  ]
}

// Horizontal bars on a log axis. `groups` is ((label, ((name, ns), ...)), ...).
#let log-bars(groups, names, w: 11.0, bar: 0.26, gap: 0.22, label-w: 3.2) = {
  let values = groups.map(g => g.at(1).map(b => b.at(1))).flatten().filter(x => x != none)
  let lo = calc.floor(calc.log(calc.min(..values), base: 10))
  let hi = calc.ceil(calc.log(calc.max(..values), base: 10))
  let X(ns) = (calc.log(ns, base: 10) - lo) / (hi - lo) * w
  let rows = groups.map(g => g.at(1).len()).sum()
  let H = rows * bar + groups.len() * gap
  canvas(length: 1cm, {
    import draw: *
    for d in range(lo, hi + 1) {
      line((X(calc.pow(10.0, d)), 0), (X(calc.pow(10.0, d)), H), stroke: 0.3pt + luma(210))
      content((X(calc.pow(10.0, d)), -0.12), anchor: "north", text(size: 6pt, fill: luma(80), fmt-time(
        calc.pow(10.0, d),
      )))
    }
    let y = H
    for (label, bars) in groups {
      y -= gap / 2
      let top = y
      for (j, (name, ns)) in bars.enumerate() {
        let c = palette.at(names.position(n => n == name))
        if ns != none {
          rect((0, y - bar * 0.85), (calc.max(X(ns), 0.02), y), fill: c, stroke: none)
          content((X(ns) + 0.08, y - bar * 0.42), anchor: "west", text(size: 6pt, fmt-time(ns)))
        }
        y -= bar
      }
      content((-0.15, (top + y) / 2), anchor: "east", text(size: 7pt, label))
      y -= gap / 2
    }
  })
}

// ---------------------------------------------------------------- document

#align(center)[
  #text(size: 18pt, weight: "bold")[yabiir benchmark history]
  #v(0.2em)
  #text(size: 11pt)[Criterion medians for v0.2.0 through v0.8.0]
  #v(0.2em)
  #text(size: 9pt, fill: luma(90))[
    #runs.at(0).cpu, CPU #runs.at(0).pinned_cpu ·
    measured #session.started_at.slice(0, 10) to #runs.at(last).measured_at.slice(0, 10)
  ]
]

= Summary

Every release from v0.2.0 to v0.8.0 was benchmarked in one interleaved campaign: each benchmark ran on all eight
releases back to back, in a shuffled order, before the next benchmark started. Most of yabiir got neither faster nor
slower: the hot paths were already settled by v0.2.0, and later work went into correctness, observability and new
features. Four changes show up clearly in the numbers:

- *v0.6.0, keydir lookups:* the keydir gained a sorted mode and became a two-way `enum`. A hashed `get` went from
  #fmt-time(med(v051, "get_hit/1000")) to #fmt-time(med(v06, "get_hit/1000")), and a miss from
  #fmt-time(med(v051, "get_miss/10000")) to #fmt-time(med(v06, "get_miss/10000")): 1.1–1.4 ns per call, for one
  branch and a larger type. Snapshots of 10,000 keys and more got 11–15% slower too.
- *v0.7.0, `read_entry`:* the entry reader stopped trusting the header's length. It reads the body through `take`,
  growing its buffer as bytes arrive, and feeds the CRC hasher the header and body separately. A small entry cost
  #fmt-pct(med(v07, "read_entry/64") / med(v06, "read_entry/64")) at 64 bytes
  (#fmt-time(med(v06, "read_entry/64")) → #fmt-time(med(v07, "read_entry/64"))).
- *v0.7.0, `merge_noop`:* from #fmt-time(med(v06, "merge_noop")) to #fmt-time(med(v07, "merge_noop")). This is not
  a slowdown: the benchmark changed meaning. Merge now seals and compacts the active file, so the "no-op" merge of 50
  keys in the active file is a real merge, with its fsyncs.
- *v0.8.0, `read_entry` again:* the fix checks a header against the bytes left in the file before allocating, and
  hashes header and body in one call. Every size is now faster than in any earlier release, by
  #fmt-pct(med(v08, "read_entry/64") / med(v06, "read_entry/64")) at 64 bytes and
  #fmt-pct(med(v08, "read_entry/65536") / med(v06, "read_entry/65536")) at 64 KiB against v0.6.0
  (@read-entry-sec).

Everything else stays within a few percent from release to release. The control benchmarks, whose code never
changed, stay within #fmt-pct(calc.max(..control)) of v0.2.0, so a move of 5% or more is worth reading.

= How the numbers were taken <measurement>

`cargo xtask campaign` built each tag once, in its own worktree, and ran the benchmark binaries under `taskset` on
CPU #runs.at(0).pinned_cpu. Every tag used the same compiler, the same release profile (`opt-level = 3`, fat LTO, one
codegen unit), Criterion 0.8.2 and crc32fast 1.5.1, and each tag ran its own benchmark code. The campaign kept
Criterion's median and its 95% confidence interval for each benchmark.

The order is what matters. A campaign is split into units, one benchmark group each, such as the five `read_entry`
sizes. A unit runs every benchmark of the group on every release, in a freshly shuffled order, then saves its
results. So the releases are compared under the same conditions, minutes apart, and whatever the machine does over
the hours lands on all of them alike. This campaign was one round, run in
one session from #session.started_at.slice(11, 16) to
#runs.at(last).measured_at.slice(11, 16) UTC: 35 units in just under four hours.

The machine was not tuned: turbo was #if session.machine.turbo [on] else [off], the governor was
"#session.machine.governor" and the measured core's sibling was online. The first history, which ran the releases one
after another, drifted under the same settings: its control index climbed steadily to +6.5% at v0.7.0, which ran last,
and that drift was larger than most of the changes it was meant to show. Criterion's intervals cannot see this: the
median confidence interval is about ±0.35% for a typical benchmark.

The *control index* measures what is left of the drift: the geometric mean, relative to v0.2.0, of the benchmarks
whose code is the same in every release. These are `verify_crc`, `encode_entry`, `decode_entry_header`,
`decode_hint_header` and `encode_hint`; `encode_entry` gained only a `debug_assert!`, which release builds drop.
@control plots it, and every chart that follows draws it as a dashed grey line. A benchmark that moves with that line
is measuring the machine.

#figure(
  [
    #ratio-panel(control-keys.map(k => (k, range(versions.len()).map(i => med(i, k) / med(0, k)))), w: 11, h: 4.2, color: palette.at(0).transparentize(40%))
    #legend((("each control benchmark", palette.at(0), false), ("control index", ref-color, true)))
  ],
  caption: [
    Control benchmarks: unchanged code, relative to v0.2.0. The index stays between
    #fmt-pct(calc.min(..control)) and #fmt-pct(calc.max(..control)); the sequential history reached +6.5%.
  ],
) <control>

One round gives one median per release, so a single odd measurement cannot be told from a real change by the
numbers alone. A few benchmarks are bimodal (@families) and should be read with that in mind. A second round would
give each release a spread as well as a median.

= Changes between versions

The commentary for each release lists the changes that could affect speed and what the benchmarks showed.

/ v0.2.0 (baseline): The engine as first released: the aHash keydir behind one `RwLock`, the append-only data
  files, recovery from hint files, merge with batched flushes, file locking, and group commit for `sync_on_put`.

/ v0.3.0: Tests for the gaps cargo-mutants found, including one real merge bug (a live entry after a corrupt one
  was lost). CI, the `xtask release` command and the docs. *Benchmarks:* flat. `get_miss/1000` reads
  #fmt-time(med(1, "get_miss/1000")) against #fmt-time(med(0, "get_miss/1000")) either side; that benchmark is
  bimodal, not slower (@families).

/ v0.4.0: Observability: a `Metrics` trait the engine calls on every operation, `tracing` behind a feature, spans
  in merge, and gauges for merge's pending queue. *Benchmarks:* flat. The keydir benchmarks don't go through the
  engine, so they can't see the per-operation timing; the merge benchmarks, which do, did not move.

/ v0.5.0: Hint files gained a whole-file trailing CRC, and recovery falls back to the data file on any fault. Some API
  tidying. *Benchmarks:* flat; recovery itself isn't benchmarked.

/ v0.5.1: The crate moved into a workspace. Two concurrency fixes for merge: a reader pins the data file's handle
  under the keydir lock, and merge waits for in-flight writes (the publish gate). *Benchmarks:* flat. The
  `read_hint` and `read_entry/64` outliers of the sequential history are gone.

/ v0.6.0: The keydir can be kept sorted (a `BTreeMap`), chosen at open, with `Engine::scan` over a key range; a new
  `keydir_order` benchmark compares the two modes. *Benchmarks:* the hashed keydir pays for the choice:
  `get_hit` #fmt-pct(gm(keys-of("get_hit"), v06, base: v051)) and `get_miss`
  #fmt-pct(gm(keys-of("get_miss").filter(k => k != "get_miss/100"), v06, base: v051)) against v0.5.1
  (@families), from the `match` on every call. `snapshot` is
  #fmt-pct(med(v06, "snapshot/10000") / med(v051, "snapshot/10000")) at 10,000 keys and
  #fmt-pct(med(v06, "snapshot/100000") / med(v051, "snapshot/100000")) at 100,000, through the iterator `enum`.
  Inserts, removes and CAS did not move, because their cost is the hashing and the allocation, not the branch.

/ v0.7.0: The largest release. The single writer became strict (a write is appended and published under one lock),
  merge seals the active file and reserves its output ids below later writes, a failed fsync fails the writes it
  covered for good, data-file creation and removal fsync the directory, and a corrupt header can no longer cause an
  unbounded allocation. The workspace split into six crates, with SQL in its own crate, and the engine, keydir,
  recovery and merge became module directories. *Benchmarks:* `read_entry` and `merge_noop` changed for the reasons
  above (@read-entry, @merge-noop). The 10-file merge moved by
  #fmt-pct(med(v07, "merge_by_input_file_count/10") / med(v06, "merge_by_input_file_count/10")), and stays there in
  v0.8.0: sealing the active file and fsyncing the directory add a fixed cost, which the larger merges hide. The rest
  is flat. The 5–18% slowdowns the sequential history showed here were the machine.

/ v0.8.0: `read_entry` rejects a header claiming more than the file holds before allocating anything, and hashes the
  header and body in one call (@read-entry-sec). *Benchmarks:* `read_entry` is the fastest it has been at every size.
  `snapshot/100` reads #fmt-pct(med(v08, "snapshot/100") / med(v07, "snapshot/100")) with no change to the keydir;
  that is code layout or a lucky run, not a finding.

= Benchmark families

Each panel shows one benchmark family, a line per parameter (value size in bytes, or key count), relative to its
v0.2.0 median, which the panel's header gives. The dashed line is the control index.

#let families = (
  ("get_hit", "keydir get, hit", " keys"),
  ("get_miss", "keydir get, miss", " keys"),
  ("insert_new_key", "keydir insert, new key", " keys"),
  ("insert_overwrite", "keydir insert, overwrite", " keys"),
  ("remove", "keydir remove", " keys"),
  ("snapshot", "keydir snapshot", " keys"),
  ("cas_repoint_success", "keydir CAS repoint, success", " keys"),
  ("cas_repoint_failure", "keydir CAS repoint, failure", " keys"),
  ("read_entry", "read_entry (decode + CRC)", " B"),
  ("active_file_read_at", "active file read_at", " B"),
  ("data_file_set_read_at_cached", "data file set read_at", " B"),
  ("append_no_sync", "append, no fsync", " B"),
  ("discover", "discover data files", " files"),
  ("merge_by_input_file_count", "merge by input file count", " files"),
  ("merge_by_versions_per_key", "merge by versions per key", " versions"),
)

#figure(
  grid(
    columns: 2,
    column-gutter: 0.6cm,
    row-gutter: 0.5cm,
    ..families.slice(0, 8).map(((p, t, u)) => family-panel(p, t, unit: u)),
  ),
  caption: [Keydir benchmark families, relative to v0.2.0.],
) <families>

#figure(
  grid(
    columns: 2,
    column-gutter: 0.6cm,
    row-gutter: 0.5cm,
    ..families.slice(8).map(((p, t, u)) => family-panel(p, t, unit: u)),
  ),
  caption: [Format, data file and merge benchmark families, relative to v0.2.0.],
) <io-families>

The keydir panels show the v0.6.0 step in `get` clearly, standing apart from the control line. `get_miss` is
bimodal at small sizes: a miss costs about #fmt-time(med(0, "get_miss/10000")), or double that, and which one a run
gets has nothing to do with the release (`get_miss/1000` at v0.3.0, `get_miss/100` at v0.4.0 and v0.5.1). This is
probably where the probed key's hash lands relative to the table's cache lines. Read the larger sizes. `remove/100`
and `insert_new_key` at small sizes flip between two levels in the same way.

The merge families are the most stable of all: merge spends its time in `write` and `fsync`, which the clock barely
affects. They stay within about 1% of v0.2.0 up to v0.6.0. From v0.7.0 the 10-file merge carries the extra fixed cost
of sealing the active file and fsyncing the directory.

= Two regressions explained

== `read_entry` <read-entry-sec>

#let re-keys = keys-of("read_entry")

#figure(
  [
    #log-bars(
      re-keys.map(k => (str(param(k)) + " B", (
        ("v0.6.0", med(v06, k)),
        ("v0.7.0", med(v07, k)),
        ("v0.8.0", med(v08, k)),
      ))),
      ("v0.6.0", "v0.7.0", "v0.8.0"),
      bar: 0.22,
    )
    #legend((
      ("v0.6.0", palette.at(0), false),
      ("v0.7.0", palette.at(1), false),
      ("v0.8.0", palette.at(2), false),
    ))
  ],
  caption: [`read_entry` by value size: v0.6.0, the v0.7.0 regression, and the v0.8.0 fix (log scale).],
) <read-entry>

An entry's lengths come from its header, and the CRC that would vouch for them covers the body too, so they are
unverified until the body has been read. A torn or corrupt header can claim about 6 GiB. Up to v0.6.0, `read_entry`
allocated whatever the header claimed, then copied the header and body into a second buffer for the CRC. On Linux,
overcommit usually lets that allocation succeed; elsewhere it aborts the process, so one torn header could stop a
store from opening.

v0.7.0 reads the body through `Read::take(len)` into an empty `Vec`, which grows only as bytes arrive, and feeds a
CRC `Hasher` the header and the body separately instead of copying them together. That costs small entries
#fmt-pct(med(v07, "read_entry/64") / med(v06, "read_entry/64")) at 64 bytes and
#fmt-pct(med(v07, "read_entry/1024") / med(v06, "read_entry/1024")) at 1 KiB; at 64 KiB the saved copy wins
(#fmt-pct(med(v07, "read_entry/65536") / med(v06, "read_entry/65536"))). `read_entry` is the scanner's inner loop, so
recovery and merge of a file of small values pay this per entry.

=== The fix, in v0.8.0

`read_entry` now takes `available`, the bytes left in the file from the entry's start. A header claiming more is
reported as a torn write before anything is allocated, and an honest one gets a single allocation, bounded by the
file. The header and body are read into one buffer and hashed in one call.

Each change was measured on its own while the fix was written. Reading into an exactly sized buffer, but still
hashing in two calls, left 64-byte entries as slow as v0.7.0. The single buffer, hashed in one call, is what
recovered the speed. In a small benchmark outside the crate, `take` with `read_to_end` was the larger cost and the
split hash cost only 10–20 ns. Inside the crate, the split-hash version stayed slow, for reasons in how it was
compiled that were not pinned down.

Against v0.6.0, the fix is #fmt-pct(med(v08, "read_entry/64") / med(v06, "read_entry/64")) at 64 bytes,
#fmt-pct(med(v08, "read_entry/1024") / med(v06, "read_entry/1024")) at 1 KiB and
#fmt-pct(med(v08, "read_entry/65536") / med(v06, "read_entry/65536")) at 64 KiB: one copy fewer than v0.6.0 and
no growth of the buffer.

== `merge_noop` <merge-noop-sec>

#figure(
  [
    #log-bars(
      versions.enumerate().map(((i, v)) => (v, (("merge_noop", med(i, "merge_noop")),))),
      ("merge_noop",),
      w: 10,
    )
  ],
  caption: [`merge_noop` as each tag defines it: a merge of a store whose 50 keys are all in the active file
    (log scale).],
) <merge-noop>

The benchmark writes 50 keys and calls `merge`. Up to v0.6.0, merge left the active file alone; with only the active
file there were no inputs, and it returned in about 4 µs. From v0.7.0 merge seals the active file first, so the 50
keys are copied into a merge output, hint file included, and fsynced, and creating and removing files fsyncs the
directory. At about 7 ms per fsync on this disk (`append_with_sync` measures
#fmt-time(med(last, "append_with_sync/1024"))), #fmt-time(med(v07, "merge_noop")) is about four of them.

After v0.8.0, `merge_noop` measures an empty store, a true no-op, and the 50-key case moved to `merge_active_only`.
The tags keep their own benchmark code, so this history shows the old meaning throughout; the next release's
history will show both.

= Hashed and sorted keydirs

#let ord(op, mode, n) = med(last, "order/" + op + "/" + mode + "/" + str(n))
#let ord-ratio(op, n) = ord(op, "sorted", n) / ord(op, "hashed", n)
#let point-ratios = { ("get_hit", "get_miss", "get_random", "overwrite").map(op => (1000, 100000, 1000000).map(n => ord-ratio(op, n))).flatten() }

`keydir_order`, new in v0.6.0, compares the two keydir modes at 1,000, 100,000 and a million keys. @order shows
v0.8.0. Point operations cost #calc.round(calc.min(..point-ratios)) to #calc.round(calc.max(..point-ratios)) times
more in the sorted keydir, and growing from 1,000 to a million keys makes its random lookups
#calc.round(ord("get_random", "sorted", 1000000) / ord("get_random", "sorted", 1000), digits: 1)× slower, while a
100-key range is #calc.round(1 / ord-ratio("range_100", 1000000) / 1000)k times faster at a million keys, because the
hashed keydir has to filter and sort every key.

#let order-ops = ("get_hit", "get_miss", "get_random", "overwrite", "range_100", "build")
#figure(
  [
    #log-bars(
      order-ops
        .map(op => (1000, 1000000).map(n => (
          op + " · " + (if n == 1000 { "1k" } else { "1M" }),
          (
            ("hashed", ord(op, "hashed", n)),
            ("sorted", ord(op, "sorted", n)),
          ),
        )))
        .join(),
      ("hashed", "sorted"),
      bar: 0.22,
      gap: 0.16,
    )
    #legend((("hashed (aHash HashMap)", palette.at(0), false), ("sorted (BTreeMap)", palette.at(1), false)))
  ],
  caption: [`keydir_order` at v0.8.0, 1,000 and 1,000,000 keys (log scale). `build` is the whole keydir
    built from scratch, as at open.],
) <order>

Across v0.6.0 to v0.8.0 the keydir code did not change, and most of these stay within a few percent. The sorted
builds at 100,000 and a million keys got faster in v0.7.0 and again in v0.8.0
(#fmt-pct(ord("build", "sorted", 100000) / med(v06, "order/build/sorted/100000")) and
#fmt-pct(ord("build", "sorted", 1000000) / med(v06, "order/build/sorted/1000000")) at v0.8.0 against v0.6.0) while
the hashed builds held still. These are long runs dominated by allocation and memory traffic, where the allocator's
state and code layout matter, so the move is real for these binaries but says nothing about the keydir itself. A
handful of the long sorted lookups at 100,000 keys move by 10–18% between neighbouring releases in either direction;
with one round, those are noise until a second round says otherwise.

#pagebreak()

= All medians

#let all-keys = runs.at(last).results.keys().filter(k => not k.starts-with("order/"))
#let order-keys = runs.at(last).results.keys().filter(k => k.starts-with("order/"))

Every median, in Criterion's unit converted for reading. A shaded cell is more than 10% slower (red) or faster
(green) than the release before, after taking out the control drift; the `keydir_order` benchmarks exist only from
v0.6.0.

#let cell(i, k) = {
  let m = med(i, k)
  if m == none { return text(fill: luma(160))[–] }
  let prev = if i > 0 { med(i - 1, k) } else { none }
  let fill = none
  if prev != none {
    let r = (m / prev) / (control.at(i) / control.at(i - 1))
    if r > 1.10 { fill = rgb("#f6d5d1") } else if r < 1 / 1.10 { fill = rgb("#d4efdc") }
  }
  table.cell(fill: fill, fmt-time(m))
}

#let medians-table(keys) = {
  set text(size: 6.5pt)
  table(
    columns: (auto,) + (1fr,) * versions.len(),
    align: (left,) + (right,) * versions.len(),
    stroke: none,
    inset: (x: 3pt, y: 2pt),
    table.hline(stroke: 0.5pt),
    table.header([*benchmark*], ..versions.map(v => [*#v*])),
    table.hline(stroke: 0.4pt),
    ..keys
      .sorted(key: k => k
        .split("/")
        .map(p => if p.match(regex("^\d+$")) != none { "0" * (10 - p.len()) + p } else { p })
        .join("/"))
      .map(k => (raw(k), ..range(versions.len()).map(i => cell(i, k))))
      .flatten(),
    table.hline(stroke: 0.5pt),
  )
}

#medians-table(all-keys)
#medians-table(order-keys)
