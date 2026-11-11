#import "../template/isss.typ": *

= The Keydir <ch-keydir>

#status[Incipient. The entry format and the keydir are walked through from a real run; the
recovery path, a figure and a measured full scan are still to come.]

I wrote yabiir without once thinking of its keydir as an index. Indexes, to me, were something a
database would need sooner or later — a nebulous later. The keydir was already one: the only index a
Bitcask store has, and the reason a lookup costs one disk read.

== Log-structured storage

A Bitcask store is a directory of append-only data files @sheehy2010bitcask:

- every `put` and every `delete` appends one _entry_ to the end of the active data file;
- when the active file reaches its size limit, it is closed, becomes immutable, and a new one opens;
- `merge` copies the live entries of the immutable files into new files and removes the old ones.

*Nothing that has been written is ever modified in place.* A new value for a key is a new entry
further along; a delete is an entry too. Justin Sheehy and David Smith designed it at Basho, for
Riak, after Eric Brewer pointed out to them that merging hash-table logs could be as fast as an
LSM-tree, or faster @sheehy2010bitcask.

== The entry format

Here's the literal data file after `put("lang", "rust")`:#footnote[Produced with yabiir's `kv`
command-line tool by #repo("book/code/ch01/run.sh"); its output is in
#repo("book/code/ch01/put-get-rm.txt").]

#output(title: "00000000000000000000.bitcask.data · 24 bytes")[```
00000000: 28a8 14d1 1dcd f36a 0400 0000 0400 0000  (......j........
00000010: 6c61 6e67 7275 7374                      langrust
```]

Twenty-four bytes, every integer little-endian. `28a814d1` is the CRC32 of the twenty bytes after
it. `1dcdf36a` is the timestamp: 1,794,362,653 seconds, 02:04 UTC on 11 November 2026. `04000000`
is the key's length and `04000000` the value's, four bytes each. Then the key, `lang`, and the value,
`rust`. Sixteen bytes of header, eight of data.

Deleting the key writes a second entry, into a different file:#footnote[Each `kv` invocation opens
the store, and opening always starts a new, empty active file. The `get` in between left
`00000000000000000001.bitcask.data` empty, because `kv` opens the store for writing even to read.]

#output(title: "00000000000000000002.bitcask.data · 20 bytes")[```
00000000: 126b 6b2f 1dcd f36a 0400 0080 0000 0000  .kk/...j........
00000010: 6c61 6e67                                lang
```]

The key length is now `04000080`: the top bit is set, which marks the entry as a _tombstone_, and the
remaining 31 bits still say four. The value length is zero and no value follows. The 24 bytes in
file 0 are unchanged. Two appends, no overwrite.

== The keydir

The _keydir_ maps every live key to the location of its newest value. In Sheehy and Smith's words,
"a keydir is simply a hash table" @sheehy2010bitcask, and in yabiir it's a Rust `HashMap`:

#srcfile("keydir.rs", lines: ((12, 19), (45, 47)), notes: (
  (17, [For `lang` after the `put`: file 0, position 16 + 4 = 20, size 4.]),
))[```rust
pub struct KeydirEntry {
    pub file_id: u32,
    pub value_size: u32,
    /// Position of the VALUE bytes (past the entry header), not the entry
    /// header itself — this is exactly what a read seeks/preads from.
    pub value_pos: u64,
    pub timestamp: u32,
}
// …
pub struct Keydir {
    map: HashMap<Box<[u8]>, KeydirEntry, AHashState>,
}
```]

A `get` is one map lookup and one positioned read of `value_size` bytes at `value_pos`. A `put`
appends, then inserts the new location. A `delete` appends a tombstone, then removes the key.

#deep(title: "The files are the truth")[
  The keydir lives only in memory. Every time yabiir opens a store it rebuilds the keydir by reading
  the data files from the oldest to the newest, letting later entries replace earlier ones and
  tombstones remove them. A data file that has a _hint file_ — written by `merge`, holding each
  entry's key and position but not its value — is read through that instead.
]

== Hash indexes

A hash map turns a key into a bucket number and goes straight to that bucket. With _n_ keys in the
store, that makes one kind of question cheap and every other kind a full walk:

#table(
  columns: (auto, auto, 1fr),
  [Question], [Cost], [How],
  [Is `k` here, and where?], [O(1)], [Hash `k`, go to its bucket.],
  [Every key, in any order], [O(n)], [Walk every bucket: `list_keys`, `fold`.],
  [Keys from `a` to `b`], [O(n log n)], [Read every key, sort, cut.],
  [Keys starting with `user/`], [O(n)], [Read every key and test each one.],
  [The 20 newest entries], [O(n log n)], [Read every key, sort, keep twenty.],
)

None of Bitcask's eight design goals mention order — the first is "low latency per item read or
written" @sheehy2010bitcask — and the design meets that goal by giving order up.

#warn(title: "Hashing destroys order on purpose")[
  A good hash function sends similar keys to unrelated buckets. FxHash, tried for yabiir's keydir
  before aHash, does too little of that: 10,000 sequential keys of the form `key-0000000042` landed in
  one of 16,384 buckets, and every keydir operation became a walk of that one bucket.#footnote[The
  measurement is recorded in the doc comment on `Keydir`, in #repo("crates/yabiir/src/keydir.rs").]
  A hash that does spread keys well leaves no trace of their order, and only sorting gets it back.
]

That's the keydir: one jump for an exact key, a walk of everything for any other question.
@ch-sorted replaces the hash map with a B-tree.

#takeaways(
  [A Bitcask store is append-only data files plus an in-memory keydir; deletes are tombstone entries.],
  [The keydir maps each live key to file, position and size, so a `get` is one lookup and one read.],
  [It's rebuilt from the files (or their hint files) at every open.],
  [As a hash map it answers "where is `k`?" in O(1) and every ordered question in O(n) or worse.],
)

#todo(
  [Recovery walked through on the same three files, including what the empty file 1 costs.],
  [A figure: data files on the left, keydir on the right, arrows for one `get`.],
  [Measure `list_keys` plus a sort on a million keys, against a single `get`.],
)
