# Bitcask Implementation Plan (Rust)

Source: `docs/papers/bitcask-intro.pdf` ("Bitcask: A Log-Structured Hash Table for
Fast Key/Value Data", Sheehy & Smith, Basho, 2010).

This plan breaks the paper's design into buildable milestones for a from-scratch
Rust implementation, living under `src/` in this crate. Each milestone is
independently testable, builds on the previous one, and the whole thing stays
compilable and runnable at every step (no long-lived "big bang" branches).

Suggested crate layout:

```
src/
  lib.rs        // public API: Bitcask, Options, Error
  format.rs     // entry/hint byte layout, CRC
  keydir.rs     // in-memory index
  datafile.rs   // active file + read handles + directory scan
  engine.rs     // put/get/delete/fold/list_keys, ties keydir+datafile together
  recovery.rs   // open()-time scan/rebuild
  merge.rs      // compaction
  lock.rs       // single-writer directory lock
  error.rs      // BitcaskError
tests/
  crud.rs
  recovery.rs
  merge.rs
  concurrency.rs
examples/
  basic_usage.rs
benches/
  throughput.rs
```

---

## 0. Core concepts recap

- A **Bitcask instance** = a directory of append-only files.
- Exactly one writer process at a time; readers may be concurrent, in this or
  other processes.
- One **active file** is open for appends; when it crosses a size threshold it
  is closed (becomes immutable) and a new active file is created.
- Every write (including deletes, via a tombstone) is an **append** to the
  active file — no in-place mutation, no seeking on write.
- An in-memory **keydir** (hash map) tracks, per key, the newest entry's
  location: `file_id, value_sz, value_pos, tstamp`.
- **Reads** = one keydir lookup + one seek + one read. No B-tree, no
  multi-level lookup — that's the whole trick.
- **Merge** compacts old immutable files into fewer files containing only live
  values, and writes a **hint file** next to each merged data file so startup
  doesn't need to re-scan value bytes.
- Crash recovery is "free": data file == commit log, so recovery is just
  rebuilding the keydir by scanning (or reading hint files). There is no
  separate WAL to replay.

---

## 1. On-disk entry format

Per the paper's diagram:

```
crc | tstamp | ksz | value_sz | key | value
```

with the note "CRC coverage" spanning from `tstamp` through `value` (i.e. the
CRC does **not** cover itself, only everything after it).

### 1.1 Byte-exact layout

All multi-byte integers little-endian (Rust's native `to_le_bytes`, cheapest
on the platforms this will run on; document the choice so it's not
accidentally flipped later).

```
offset  size  field
0       4     crc         (u32, CRC32 of bytes [4..end) of this entry)
4       4     tstamp      (u32, unix seconds)
8       4     ksz         (u32, length of key in bytes)
12      4     value_sz    (u32, length of value in bytes)
16      ksz   key
16+ksz  value_sz  value
```

`HEADER_SIZE = 16`. Total on-disk entry size = `16 + ksz + value_sz`.

Design choices worth pinning down explicitly (the paper leaves these as
implementation details):

- **`ksz`/`value_sz` as `u32`**: caps keys/values at 4 GiB each, which is
  ample; keeps the header fixed-size and simple to `read_exact` in one shot.
- **`tstamp` as `u32` seconds**: matches the paper's diagram ("32-bit int
  local timestamp (internal-only, not exposed)"). Not exposed via the public
  API — it exists purely so multiple writers of the *same* key across a
  crash/restart or clock skew scenario have a well-defined tie-breaker if
  ever needed (in this design we don't actually need it for correctness,
  since file_id+offset ordering already gives us a total order — keep
  `tstamp` for format fidelity and for future use, e.g. TTL/expiry).
- **CRC32, not CRC32C**: use whichever the chosen crate makes easiest
  (`crc32fast` implements the standard IEEE CRC32 with SIMD acceleration).
  Consistency matters more than which polynomial.

### 1.2 Tombstone representation

The paper: "deletion is simply a write of a special tombstone value, which
will be removed on the next merge." Concretely:

Three options were considered, in order of increasing quality:

1. **Magic length sentinel** (`value_sz == u32::MAX`, or the mirror-image
   `ksz == u32::MAX`): rejected. `value_sz`-as-sentinel is ambiguous with a
   real large value unless you forbid that exact length; `ksz`-as-sentinel
   works but forces the *next* field (`value_sz`) to be reinterpreted as the
   real key length purely for tombstones, which means the meaning of a
   header field depends on a flag elsewhere in the header — more special
   -casing than necessary.
2. **A dedicated 1-byte `flags` field**: correct and unambiguous, but costs a
   full extra byte on *every* entry (not just tombstones) for a single bit of
   information, and grows the header to 17 bytes.
3. **Steal the top bit of `ksz`** (chosen): a 32-bit key length is already
   absurd headroom — no realistic key is anywhere near 2 GiB, let alone 4 GiB
   — so the top bit can be repurposed as the tombstone flag at zero practical
   cost to key capacity, while leaving `value_sz` with its single, unambiguous
   meaning (real values, unlike keys, are the field where someone might
   plausibly want the full 4 GiB range someday, so don't steal a bit there).

Revised header (still 16 bytes — **no** separate flags byte):

```
offset  size  field
0       4     crc
4       4     tstamp
8       4     ksz_and_flags   (bit 31 = TOMBSTONE; bits 0..30 = real key length)
12      4     value_sz        (0 for tombstones — deletes carry no payload)
16      ksz   key              (ksz = ksz_and_flags & 0x7FFF_FFFF)
16+ksz  value_sz  value
```

`HEADER_SIZE = 16`. Encode/decode helpers:

```rust
const TOMBSTONE_BIT: u32 = 1 << 31;
const KSZ_MASK: u32 = !TOMBSTONE_BIT;

fn encode_ksz(key_len: u32, tombstone: bool) -> u32 {
    debug_assert!(key_len <= KSZ_MASK, "key too large ({key_len} bytes, max {KSZ_MASK})");
    key_len | if tombstone { TOMBSTONE_BIT } else { 0 }
}

fn decode_ksz(raw: u32) -> (u32 /* key_len */, bool /* tombstone */) {
    (raw & KSZ_MASK, raw & TOMBSTONE_BIT != 0)
}
```

This caps real key length at `2^31 - 1` (≈2.1 GiB) instead of `2^32 - 1`
(≈4.3 GiB) — a purely theoretical loss given any reasonable deployment caps
key size far lower than that anyway (worth adding an explicit, much smaller
`max_key_size` check in the engine regardless, e.g. reject keys over a few
hundred KiB, independent of this format-level ceiling).

Parsing stays a simple mask-and-check rather than "does this field mean what
it normally means, or something else" branching, which is what made the
`ksz == u32::MAX` variant worse: here `value_sz` is *always* the real value
length (0 for tombstones, since there's never any payload to store), full
stop — no reinterpretation of any field based on another field's value.

### 1.3 Hint file entry format

Hint files exist purely so startup doesn't need to read value bytes. Per the
paper's diagram (`tstamp | ksz | value_sz | value_pos | key`):

```
offset  size  field
0       4     tstamp
4       4     ksz_and_flags   (same bit-31-is-tombstone encoding as the data
                                file header; merge never writes tombstones to
                                hint files since it drops dead entries during
                                compaction, but keep the bit for format
                                symmetry / future use)
8       4     value_sz
12      8     value_pos    (u64 — offset of the *value* within the data file,
                             i.e. header_end for that entry, not the entry start)
20      ksz   key
```

`HINT_HEADER_SIZE = 20`. No CRC in the hint file — it's a derived/rebuildable
artifact; if it's corrupt or missing, fall back to scanning the data file.

### 1.4 `format.rs` deliverables

```rust
pub struct EntryHeader {
    pub tstamp: u32,
    pub ksz: u32,
    pub value_sz: u32,
    pub tombstone: bool,
}

pub struct Entry {
    pub header: EntryHeader,
    pub key: Vec<u8>,
    pub value: Vec<u8>, // empty for tombstones
}

pub const HEADER_SIZE: usize = 16;

/// A fully-encoded data-file entry (CRC + header + key + value). A distinct
/// type from `EncodedHint` — both are "just bytes" on disk, but they go to
/// different files with different layouts, so keeping them as separate
/// Rust types makes it a compile error to append one where the other
/// belongs. Also carries `value_len` so callers (`ActiveFile::append`)
/// don't need a second, separately-threaded length parameter that could
/// drift out of sync with the buffer.
pub struct EncodedEntry { /* bytes: Vec<u8>, value_len: usize */ }
impl EncodedEntry {
    pub fn as_bytes(&self) -> &[u8];
    pub fn value_len(&self) -> usize;
}

pub fn encode_entry(key: &[u8], value: &[u8], tombstone: bool, tstamp: u32) -> EncodedEntry;
// writes crc+header+key+value into one buffer, ready to append

pub fn decode_entry_header(buf: &[u8; HEADER_SIZE]) -> (u32 /*crc*/, EntryHeader);

pub fn verify_crc(crc: u32, rest_of_entry: &[u8]) -> bool;

// Hint file symmetric helpers:
pub const HINT_HEADER_SIZE: usize = 20;

/// A fully-encoded hint-file record. Same rationale as `EncodedEntry` for
/// being its own type rather than a bare `Vec<u8>`.
pub struct EncodedHint { /* bytes: Vec<u8> */ }
impl EncodedHint {
    pub fn as_bytes(&self) -> &[u8];
}

pub fn encode_hint(key: &[u8], header: &EntryHeader, value_pos: u64) -> EncodedHint;
pub fn decode_hint_header(buf: &[u8; HINT_HEADER_SIZE]) -> (EntryHeader, u64 /*value_pos*/);
```

Implementation notes:
- `encode_entry` builds the buffer tail-first (key+value+header-minus-crc),
  computes CRC over that, then prepends the CRC — or just build the whole
  buffer and compute CRC over `buf[4..]` in place. Either is fine; prefer
  building into a single `Vec<u8>` (or a reusable `BytesMut`-style buffer
  owned by `ActiveFile` later, to avoid per-write allocation — note as a
  later optimization, not required for v1).
- `decode_entry_header` takes a fixed `[u8; 16]` so callers do one
  `read_exact` for the header, then know exactly how many more bytes
  (`ksz + value_sz`) to `read_exact` for the body — no length-prefixed
  parsing ambiguity.
- Provide a `read_entry_at<R: Read + Seek>(r: &mut R, offset: u64) -> io::Result<Option<Entry>>` convenience that seeks, reads header, reads body, verifies CRC, and returns `Ok(None)` on a **clean EOF exactly at entry start** (used by recovery to detect "no more entries") vs. `Err` on a **partial/truncated entry** (used by recovery to detect "crashed mid-write, stop here").

### 1.5 Tests for this step

- Round-trip: encode a handful of (key, value, tombstone) combinations
  including empty key... actually keys must be non-empty (reject `ksz == 0`
  at the API layer, not the format layer — format layer should just encode
  whatever it's given), empty value, and large (1 MiB) value; decode and
  assert equality.
- CRC detection: encode an entry, flip one bit in the buffer, assert
  `verify_crc` (or `read_entry_at`) reports corruption rather than silently
  returning wrong data.
- Truncation detection: encode an entry, truncate the buffer at every
  possible byte boundary from 0 to `len-1`, assert `read_entry_at` treats
  truncation *within* the header specially (can't even know how much body to
  expect) vs. truncation *within* the body (knows expected length, got less)
  — both should surface as "incomplete", never as a false-successful decode.
- Add `crc32fast = "1"` to `Cargo.toml`.

---

## 2. Keydir

### 2.1 Data structure

```rust
#[derive(Clone, Copy)]
pub struct KeydirEntry {
    pub file_id: u32,
    pub value_sz: u32,
    pub value_pos: u64, // position of the VALUE bytes, not the entry header
    pub tstamp: u32,
}

pub struct Keydir {
    map: std::collections::HashMap<Box<[u8]>, KeydirEntry>,
}
```

Why `Box<[u8]>` for the key rather than `Vec<u8>`: same semantics, slightly
smaller (no separate capacity field), and communicates "this is an immutable
stored key" — a minor detail worth getting right once since every key in the
whole dataset pays this cost.

### 2.2 API

```rust
impl Keydir {
    pub fn new() -> Self;
    pub fn get(&self, key: &[u8]) -> Option<KeydirEntry>;
    pub fn insert(&mut self, key: &[u8], entry: KeydirEntry);
    pub fn remove(&mut self, key: &[u8]) -> Option<KeydirEntry>;
    pub fn len(&self) -> usize;
    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &KeydirEntry)>;
    /// Compare-and-repoint used by merge: only overwrite if the entry is
    /// still exactly what merge observed when it copied this key forward
    /// (i.e. no newer write raced ahead of the merge for this key).
    pub fn cas_repoint(&mut self, key: &[u8], expected_old: KeydirEntry, new: KeydirEntry) -> bool;
}
```

`cas_repoint` matters for §7 (merge) correctness — see that section for why a
blind overwrite during merge repoint is wrong.

### 2.3 Concurrency wrapper

The `Keydir` itself is *not* thread-safe; wrap it once, at the engine level:

```rust
pub(crate) struct SharedKeydir(std::sync::RwLock<Keydir>);
```

Readers (`get`, `fold`, `list_keys`) take a read lock; writers (`insert`,
`remove`, `cas_repoint`) take a write lock. A single `RwLock` around a
`HashMap` is the right starting point — don't reach for `DashMap` or
sharding until a benchmark actually shows lock contention, per the "don't
build for hypothetical scale" principle.

### 2.4 Tests

- Insert then get returns the same entry; insert same key twice, get returns
  the *second* entry (last-write-wins).
- Remove then get returns `None`.
- `cas_repoint` succeeds only when `expected_old` matches current state,
  fails (returns `false`, doesn't mutate) otherwise — test both branches
  explicitly.
- A threaded test: N reader threads calling `get` in a loop while 1 writer
  thread calls `insert`/`remove` in a loop for a fixed duration; assert no
  panics/deadlocks and that the map is left in a consistent state (every
  key present maps to *some* valid-looking entry, never garbage).

---

## 3. Data file management

### 3.1 File naming and discovery

- Data files: `{file_id:020}.bitcask.data` (zero-padded decimal, `file_id:
  u64` — u32 was fine for the keydir's in-memory field but on-disk IDs should
  have headroom; keep the keydir's `file_id` as `u32` only if you're sure
  you'll never exceed 4 billion files, which is safe in practice given each
  file is at minimum megabytes — but simplest is to just use `u32`
  consistently everywhere and not worry about it).
- Hint files: `{file_id:020}.bitcask.hint`, same `file_id`, one-to-one with a
  data file (a data file may exist without a hint file; a hint file should
  never exist without its data file — if it does on open, log a warning and
  ignore the orphaned hint file).
- On `open()`, list the directory, filter to `*.bitcask.data` entries, parse
  `file_id` from each name (skip/warn on anything that doesn't match the
  pattern rather than erroring the whole open — forward-compatible with e.g.
  a future lockfile or metadata file living in the same directory), sort
  ascending by `file_id`.

### 3.2 `ActiveFile`

```rust
pub struct ActiveFile {
    file_id: u32,
    writer: std::io::BufWriter<std::fs::File>,
    offset: u64, // current end-of-file / next append position
}

impl ActiveFile {
    pub fn create(dir: &Path, file_id: u32) -> io::Result<Self>;
    /// Appends one pre-encoded entry (from format::encode_entry), returns
    /// (file_id, value_pos, entry_total_len). Takes `&EncodedEntry` rather
    /// than a bare `&[u8]` + separate length so the value length always
    /// travels with the bytes it describes — see format.rs's `EncodedEntry`.
    pub fn append(&mut self, encoded: &format::EncodedEntry) -> io::Result<(u32, u64, u64)>;
    pub fn sync(&mut self) -> io::Result<()>; // flush BufWriter + File::sync_data
    pub fn len(&self) -> u64; // == offset, used for rotation threshold check
}
```

Key correctness details:
- `append` must compute `value_pos` as `self.offset + (encoded.len() -
  encoded.value_len())`, i.e. the position where the *value* bytes start
  within the file — that's what the keydir stores and what reads seek to
  directly (skipping the header on every read).
- After writing, `self.offset += encoded.len() as u64`.
- Open the underlying `File` with `.append(true)` is tempting but don't rely
  on OS-level O_APPEND for the offset bookkeeping — track `offset` explicitly
  in Rust so `append`'s return value is always correct even if something
  else about the file mode changes later. Still fine to also set
  `.append(true)` as a defense-in-depth measure against accidental
  out-of-order writes.
- `sync()`: `BufWriter::flush()` then `File::sync_data()` (not
  `sync_all()` — we don't need metadata durability like mtime, just data +
  length; `sync_data` is cheaper). Call this on every `put` when
  `Options::sync_on_put` is set, and always on `close()`/rotation.

### 3.3 Read-side file handles

```rust
pub struct DataFileSet {
    dir: PathBuf,
    // read-only handles for immutable files, opened lazily and cached
    readers: std::sync::Mutex<std::collections::HashMap<u32, std::fs::File>>,
}

impl DataFileSet {
    pub fn discover(dir: &Path) -> io::Result<Vec<u32>>; // sorted file_ids found
    pub fn read_at(&self, file_id: u32, pos: u64, len: u32) -> io::Result<Vec<u8>>;
    pub fn hint_path(dir: &Path, file_id: u32) -> PathBuf;
    pub fn data_path(dir: &Path, file_id: u32) -> PathBuf;
}
```

- `read_at` uses `File::read_exact_at` (Unix `pread`-based, via
  `std::os::unix::fs::FileExt`) rather than seek+read, so concurrent readers
  on the *same* cached `File` handle don't race on a shared file cursor. This
  is the single most important correctness detail for concurrent reads —
  using `Seek` + `Read` on a shared handle across threads is a classic bug.
  On non-Unix targets fall back to a per-call `File::open` (no shared handle,
  no cursor race, just slightly slower) or use `positioned-io`/`pread2` crate
  for portability — decide based on whether Windows support matters; if not,
  gate this behind `#[cfg(unix)]` and document it.
- Cache eviction: start with "never evict, just grow the map" (a real
  Bitcask directory has at most a few thousand files even at large scale
  because of merge); revisit only if FD exhaustion becomes an actual problem
  in testing.
- The **active file** is never read through `DataFileSet` while it's still
  active — reads of keys most recently written go through the same
  `ActiveFile`'s underlying `File` via a second read-only fd opened
  alongside it (or, simplest: `ActiveFile` also exposes a `read_at` using its
  own file, opened once in dual read+append mode). Once a file rotates out
  of active status, it becomes reachable via `DataFileSet` like any other
  immutable file.

### 3.4 Rotation

Rotation is a **write-path** concern (triggered from `put`/`delete` in
engine.rs, not internal to `ActiveFile`), because rotating requires:
1. `active.sync()` (flush + fsync the outgoing file — don't leave a closed
   file with buffered-but-unflushed data).
2. Drop the old `ActiveFile` (closes the write handle; the file is now
   immutable and reachable for reads via `DataFileSet`).
3. `ActiveFile::create(dir, next_file_id)`.
4. `next_file_id` is a monotonic counter owned by the engine, initialized at
   `open()` time to `max(existing file_ids) + 1` (see §6).

Threshold check: after each successful append, if `active.len() >=
opts.max_file_size`, rotate immediately (before returning from `put`/
`delete`) rather than lazily on the *next* write — keeps `ActiveFile::len()`
never wildly over threshold and keeps the rotation logic in one obvious spot.

### 3.5 Tests

- Append 3 entries to a fresh `ActiveFile`, read each back via
  `DataFileSet::read_at` (after dropping/reopening as immutable) and confirm
  bytes match.
- Rotation: set a tiny `max_file_size` (e.g. 200 bytes), write enough entries
  to force 3-4 rotations, assert the directory contains the expected number
  of `*.bitcask.data` files with strictly increasing IDs and each (except the
  last) is at or above threshold.
- Concurrent-read stress: spawn several threads doing `read_at` on the same
  `DataFileSet`/file_id at different offsets simultaneously; assert each
  thread gets exactly the bytes it asked for (catches the pread-vs-shared-
  cursor bug class directly).

---

## 4. Put / Get / Delete (single-writer engine core)

### 4.1 Engine struct

```rust
pub struct Bitcask {
    dir: PathBuf,
    keydir: SharedKeydir,
    files: DataFileSet,
    active: std::sync::Mutex<ActiveFile>,
    next_file_id: std::sync::atomic::AtomicU32,
    opts: Options,
    _write_lock: Option<lock::DirLock>, // held for lifetime of a read_write open
}
```

### 4.2 `put`

```
fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
    require(!key.is_empty(), Error::EmptyKey)?;
    let tstamp = now_unix();
    let encoded = format::encode_entry(key, value, /*tombstone=*/false, tstamp);

    let mut active = self.active.lock().unwrap();
    let (file_id, value_pos, _total_len) = active.append(&encoded)?;
    if self.opts.sync_on_put {
        active.sync()?;
    }
    let needs_rotation = active.len() >= self.opts.max_file_size;
    if needs_rotation {
        active.sync()?;
        let new_id = self.next_file_id.fetch_add(1, Ordering::SeqCst);
        *active = ActiveFile::create(&self.dir, new_id)?;
    }
    drop(active); // release before touching keydir if keydir has its own lock

    self.keydir.insert(key, KeydirEntry { file_id, value_sz: value_len as u32, value_pos, tstamp });
    Ok(())
}
```

Ordering note: append-to-disk happens **before** the keydir update. This
matters for crash semantics — if we crash between the disk append and the
keydir update, on restart recovery re-scans the file and rebuilds the keydir
from what's on disk anyway, so the in-memory keydir update ordering relative
to a crash is irrelevant *for durability*, but doing disk-write-then-keydir-
update (rather than the reverse) means a concurrent reader can never observe
a keydir entry pointing at a byte range that hasn't been written yet — that's
the real reason for this order, not crash safety.

### 4.3 `get`

```
fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
    let Some(entry) = self.keydir.get(key) else { return Ok(None) };
    let raw = self.read_raw_entry(entry)?; // seeks/pread's HEADER_SIZE-worth before value_pos too, OR just the value if we trust the keydir
    Ok(Some(raw))
}
```

Two viable strategies for the actual disk read — pick one and be consistent:

- **(a) Value-only read** (matches the paper's description literally: "read
  the data using the file id, position, and size"): `value_pos` in the
  keydir already points past the header, so `read_at(file_id, value_pos,
  value_sz)` reads *only* the value bytes, no header, no CRC check on the
  read path. Fastest — exactly the promised "single seek" with minimal bytes
  transferred.
- **(b) Full-entry read + CRC re-verify**: seek to `value_pos - HEADER_SIZE`,
  read `HEADER_SIZE + value_sz` bytes, re-verify CRC. Costs a few extra bytes
  and a CRC32 computation per read, but catches on-disk bitrot on the read
  path rather than only at merge/recovery time.

Recommendation: implement **(a)** as the default (matches the paper, matches
the performance goals), but make it easy to add **(b)** later behind an
`Options::verify_crc_on_read: bool` flag defaulting to `false`. Don't build
this flag speculatively in v1 — add it only once (a) is working and tested,
as a small follow-up, since it's a trivial addition once `read_raw_entry`
exists.

### 4.4 `delete`

```
fn delete(&self, key: &[u8]) -> Result<()> {
    require(!key.is_empty(), Error::EmptyKey)?;
    if self.keydir.get(key).is_none() {
        return Ok(()); // deleting a non-existent key is a no-op, not an error — matches typical KV semantics
    }
    let tstamp = now_unix();
    let encoded = format::encode_entry(key, &[], /*tombstone=*/true, tstamp);
    let mut active = self.active.lock().unwrap();
    active.append(&encoded)?;
    if self.opts.sync_on_put { active.sync()?; }
    // (rotation check identical to put, factor into a shared helper)
    drop(active);
    self.keydir.remove(key);
    Ok(())
}
```

Note: `delete` still needs to append the tombstone *even though we're about
to remove the key from the in-memory keydir* — the tombstone on disk is what
lets a **fresh process that reopens this directory later** learn "this key
was explicitly deleted, not just absent from the files it happens to scan"
during recovery (§6.3 covers why this matters when a hint-less older file
still has a stale value for the key).

### 4.5 Tests

- `put` then `get` returns the value.
- `put` twice on the same key, `get` returns the second value only.
- `delete` then `get` returns `None`.
- `delete` a never-written key is a silent no-op (no error, no panic).
- Values of size 0, 1, and a few MiB round-trip correctly.
- Many distinct keys (e.g. 10,000) all round-trip correctly (catches hash
  collisions / iteration bugs early, cheap to run).

---

## 5. `list_keys` and `fold`

```rust
pub fn list_keys(&self) -> Vec<Vec<u8>> {
    self.keydir.iter().map(|(k, _)| k.to_vec()).collect()
}

pub fn fold<A>(&self, mut f: impl FnMut(&[u8], &[u8], A) -> A, init: A) -> Result<A> {
    let mut acc = init;
    // snapshot the (key, KeydirEntry) pairs first, then release the keydir
    // lock before doing disk I/O per entry — holding a read lock across
    // potentially thousands of disk reads would starve the writer for the
    // whole fold, which is a real usability problem, not just a style nit.
    let snapshot: Vec<(Box<[u8]>, KeydirEntry)> = self.keydir.snapshot();
    for (key, entry) in snapshot {
        let value = self.read_raw_entry(entry)?;
        acc = f(&key, &value, acc);
    }
    Ok(acc)
}
```

Important subtlety: because the snapshot is taken once and then I/O happens
without holding the keydir lock, a concurrent `merge` could delete/rename the
underlying file for an entry in the snapshot between snapshot-time and
read-time. Two ways to handle it, pick one for v1:

- **Simplest**: `DataFileSet::read_at` on a file_id that's been removed
  returns an I/O error; `fold` propagates it as `Err`. Acceptable for v1 —
  document that `fold`/`list_keys` are point-in-time snapshots and can race
  with concurrent merges in rare cases.
- **Better** (defer to a later pass, note here so it's not forgotten): don't
  unlink old files until no in-flight `read_at`/`fold` could still reference
  them — e.g. simple epoch/refcount scheme, or just rely on POSIX unlink
  semantics (open fds keep the file alive even after unlink) *if* the merge
  code keeps its own already-open handles rather than closing and deleting —
  but `fold`'s snapshot might open a *fresh* handle to a file that's already
  gone from disk. Worth a `# Deferred` callout rather than solving now.

### 5.1 Tests

- `list_keys` returns exactly the set of currently-live keys (matches a
  `HashSet` reference built from the same put/delete sequence).
- `fold` summing value lengths matches summing `get(key).len()` for every key
  in `list_keys`.
- `fold` over an empty datastore returns `init` unchanged.

---

## 6. Startup / recovery (`open()`)

### 6.1 Algorithm

```
fn open(dir: &Path, opts: Options) -> Result<Bitcask> {
    fs::create_dir_all(dir)?;
    let write_lock = if opts.read_write { Some(DirLock::acquire(dir)?) } else { None };

    let file_ids = DataFileSet::discover(dir)?; // sorted ascending
    let mut keydir = Keydir::new();

    for &file_id in &file_ids {
        let hint_path = DataFileSet::hint_path(dir, file_id);
        if hint_path.exists() {
            scan_hint_file(&hint_path, file_id, &mut keydir)?;
        } else {
            scan_data_file(dir, file_id, &mut keydir)?;
        }
    }

    let next_id = file_ids.last().map_or(0, |id| id + 1);
    let active = if opts.read_write {
        ActiveFile::create(dir, next_id)?
    } else {
        ActiveFile::none_for_read_only()
    };

    Ok(Bitcask {
        dir: dir.to_path_buf(),
        keydir: SharedKeydir::new(keydir),
        files: DataFileSet::new(dir),
        active: Mutex::new(active),
        next_file_id: AtomicU32::new(next_id + 1),
        opts,
        _write_lock: write_lock,
    })
}
```

### 6.2 `scan_data_file` (no hint file available — slow path)

```
fn scan_data_file(dir, file_id, keydir) -> Result<()> {
    let mut f = File::open(data_path(dir, file_id))?;
    let mut pos: u64 = 0;
    loop {
        match format::read_entry_at(&mut f, pos)? {
            None => break, // clean EOF exactly at an entry boundary — done
            Some(EntryRead::Truncated) => {
                // crash mid-write on the LAST file only is expected;
                // on any earlier (already-rotated) file this indicates
                // real corruption — still handle both the same way for v1
                // (stop scanning, keep everything decoded so far), but log
                // a warning in the non-last-file case.
                break;
            }
            Some(EntryRead::Ok(entry, total_len)) => {
                apply_entry_to_keydir(keydir, file_id, pos, entry);
                pos += total_len;
            }
            Some(EntryRead::CrcMismatch { total_len, .. }) => {
                // Corrupt-but-complete entry (bitrot, not a torn write).
                // Skip it (don't apply to keydir) but keep scanning past it
                // — a single corrupt entry shouldn't take down every later
                // entry in the file. Log a warning with file_id+pos.
                pos += total_len;
            }
        }
    }
    Ok(())
}

fn apply_entry_to_keydir(keydir, file_id, entry_start_pos, entry) {
    let value_pos = entry_start_pos + HEADER_SIZE as u64 + entry.header.ksz as u64;
    if entry.header.tombstone {
        keydir.remove(&entry.key);
    } else {
        keydir.insert(&entry.key, KeydirEntry {
            file_id, value_sz: entry.header.value_sz, value_pos, tstamp: entry.header.tstamp,
        });
    }
}
```

Because files are scanned in ascending `file_id` order and within a file in
ascending offset order, and `apply_entry_to_keydir` always **overwrites**
(insert) or **removes**, the final keydir state after scanning everything
naturally reflects "last write wins" — no special-casing needed, it falls
out of the scan order. This is the single most important invariant to get
right in this section.

### 6.3 `scan_hint_file` (fast path)

```
fn scan_hint_file(hint_path, file_id, keydir) -> Result<()> {
    let mut f = File::open(hint_path)?;
    loop {
        match format::read_hint_at(&mut f)? {
            None => break,
            Some(HintRead::Truncated) => break, // shouldn't happen for a
                // hint file written by a completed merge, but handle
                // gracefully — treat like data-file truncation
            Some(HintRead::Ok(header, value_pos, key)) => {
                if header.tombstone {
                    keydir.remove(&key); // see note below — currently merge
                                          // never writes tombstones to hint
                                          // files, so this branch is dead
                                          // code today but kept for format
                                          // symmetry / future-proofing
                } else {
                    keydir.insert(&key, KeydirEntry {
                        file_id, value_sz: header.value_sz, value_pos, tstamp: header.tstamp,
                    });
                }
            }
        }
    }
    Ok(())
}
```

### 6.4 Why "always start a fresh active file id on open" is correct

The paper's model says closed files are never reopened for writing. If the
process crashed while `file_id = N` was active, on restart file `N` may have
a torn last entry. Two options:
1. Treat `N` as immutable-but-possibly-torn: scan it with the same
   truncation handling as §6.2 (stops cleanly at the torn entry, keeps
   everything before it), and start a **new** active file `N+1`. This is
   simpler and is what's specified above.
2. Try to "resume" writing to `N` after truncating its torn tail. Slightly
   more space-efficient (no small leftover file), meaningfully more complex
   and easy to get wrong (must truncate the file to the last good entry
   boundary before reopening for append, must handle the truncation itself
   crashing, etc).

Go with (1) for v1 — call out (2) as a possible future optimization, not
worth the complexity now.

### 6.5 Tests

- **Full-scan recovery**: hand-write (using `format::encode_entry` directly,
  not through `Bitcask::put`, so the test doesn't depend on the engine
  already working) 2 data files with several overlapping keys across them,
  no hint files, open a `Bitcask` read-only and assert `get` returns the
  expected last-write-wins values.
- **Hint-based recovery**: same setup, but also hand-write a valid hint file
  for the older data file; assert recovery produces an identical keydir
  (compare via `list_keys` + `get` for every key) to the full-scan case —
  this is the key equivalence property to test explicitly.
- **Truncated tail**: hand-write a data file, then truncate it a few bytes
  into the last entry's value; open; assert every entry *before* the
  truncated one is recovered and the datastore opens successfully (no
  error), and a new active file was created above the truncated file's id.
- **Corrupt entry mid-file**: hand-write a data file, flip a bit inside one
  entry's value (CRC will mismatch), leave entries after it intact; assert
  that entry is skipped but entries before and after it recover correctly.
- **Crash-and-reopen integration test**: use the real `Bitcask::put` API to
  write N entries, `std::mem::forget` (or just drop without calling
  `close()`) the handle to simulate an unclean shutdown, reopen the
  directory, assert all N entries are present and correct.
- **Orphaned hint file**: a `.bitcask.hint` with no matching `.bitcask.data`
  — assert open() succeeds and simply ignores it (with a warning), rather
  than erroring.

---

## 7. Merge (compaction) + hint files

### 7.1 What "live" means, precisely

An entry at `(file_id=F, offset=O)` for key `K` is **live** iff the current
keydir entry for `K` is exactly `(file_id=F, value_pos=O + HEADER_SIZE +
ksz)`. Anything else — a superseded older version of `K`, or a tombstone (
tombstones are never "live" since a live tombstone by definition means the
key is deleted and shouldn't be in the keydir at all) — is dead and can be
dropped during merge.

### 7.2 Algorithm

```
fn merge(&self) -> Result<()> {
    // 1. Snapshot which files are eligible: everything except the CURRENT
    //    active file id at the moment merge starts. New files created by
    //    rotation *during* the merge (because puts are still happening
    //    concurrently) are simply not included in this merge pass — they'll
    //    be picked up by the next merge. This is safe and simple.
    let active_id_at_start = self.current_active_id();
    let input_ids: Vec<u32> = self.files.discover_data_file_ids()?
        .into_iter()
        .filter(|&id| id < active_id_at_start)
        .collect();
    if input_ids.is_empty() { return Ok(()); } // nothing to do

    // 2. Iterate input files oldest -> newest, entry by entry, and copy
    //    forward only the live ones into a fresh set of merge-output files.
    let mut out = MergeOutputWriter::new(&self.dir, self.next_file_id.clone());
    for &file_id in &input_ids {
        for (offset, entry) in iter_entries(&self.dir, file_id)? {
            if entry.header.tombstone { continue; } // dead by definition
            let is_live = self.keydir.get(&entry.key).map_or(false, |kd| {
                kd.file_id == file_id && kd.value_pos == offset + HEADER_SIZE as u64 + entry.header.ksz as u64
            });
            if !is_live { continue; }
            let (new_file_id, new_value_pos) = out.write_live_entry(&entry)?;
            // 3. Repoint the keydir ONLY IF it still points at the exact
            //    (file_id, offset) we just copied — if a concurrent put()
            //    already overwrote this key while we were mid-merge, our
            //    copy is now stale and must NOT clobber the newer entry.
            let old = KeydirEntry { file_id, value_sz: entry.header.value_sz, value_pos: offset + HEADER_SIZE as u64 + entry.header.ksz as u64, tstamp: entry.header.tstamp };
            let new = KeydirEntry { file_id: new_file_id, value_sz: entry.header.value_sz, value_pos: new_value_pos, tstamp: entry.header.tstamp };
            self.keydir.cas_repoint(&entry.key, old, new); // ignore false return — means a racing put already won, which is correct: our stale copy stays orphaned in the new file and is simply never pointed to, cleaned up by the *next* merge
        }
    }
    out.finish()?; // flush + fsync all output data files and hint files

    // 4. Remove the old input files now that nothing in the keydir points
    //    at them anymore (every key that was live in them now points at
    //    `out`'s files; every key that wasn't live was already pointing
    //    elsewhere and still does).
    for &file_id in &input_ids {
        let _ = fs::remove_file(self.files.data_path(&self.dir, file_id));
        let _ = fs::remove_file(self.files.hint_path(&self.dir, file_id));
    }
    Ok(())
}
```

### 7.3 Why the CAS-repoint (not a blind overwrite) matters

Concrete race this prevents: merge is copying key `K`'s live entry from old
file `F1` to new file `F3`. Between merge reading `K`'s value out of `F1` and
merge finishing the write into `F3`, a concurrent `put(K, new_value)`
happens, writing to the *current* active file and updating the keydir to
point there. If merge then did a blind `keydir.insert(K, points-at-F3)`, it
would silently resurrect the *old* value and lose the concurrent write. The
CAS (`cas_repoint`, only succeeds if the keydir entry is still exactly what
merge observed) prevents this: merge's write to `K` in `F3` simply becomes an
orphan (nothing points to it), harmless, and gets cleaned up by the *next*
merge pass (since the next merge will see `K`'s live location is now in the
new active file, not in `F3`, so `F3`'s copy of `K` won't be considered live
and won't be copied forward again).

### 7.4 `MergeOutputWriter`

Thin wrapper reusing `ActiveFile` machinery: opens a new data file (`next
merge output id`), appends entries the same way `put` does but without
touching the keydir directly (merge handles keydir updates itself per-entry
as shown above), and *also* appends the corresponding hint-format record to
a parallel hint file writer. Rotates on the same `max_file_size` threshold as
normal active files, opening `output_id, output_id+1, ...` as needed — each
rotated-in output file gets its own hint file too.

```rust
struct MergeOutputWriter<'a> {
    dir: &'a Path,
    next_id: &'a AtomicU32,
    current_data: ActiveFile,
    current_hint: BufWriter<File>,
}
impl<'a> MergeOutputWriter<'a> {
    fn write_live_entry(&mut self, entry: &Entry) -> io::Result<(u32, u64)> {
        let encoded = format::encode_entry(&entry.key, &entry.value, false, entry.header.tstamp);
        let (file_id, value_pos, _) = self.current_data.append(&encoded)?;
        let hint = format::encode_hint(&entry.key, &entry.header, value_pos);
        self.current_hint.write_all(hint.as_bytes())?;
        if self.current_data.len() >= MAX_MERGE_OUTPUT_FILE_SIZE { self.rotate()?; }
        Ok((file_id, value_pos))
    }
    fn finish(mut self) -> io::Result<()> {
        self.current_data.sync()?;
        self.current_hint.flush()?;
        Ok(())
    }
}
```

### 7.5 Tests

- **Basic compaction**: write key `A` three times (creating 3 versions
  across, once file rotation threshold is small, 3 separate files), merge,
  assert: only the last value of `A` is present on disk (verify by counting
  matching entries via a raw scan of the resulting files), `get(A)` still
  returns the latest value, old files are gone, a hint file exists for the
  new merged file.
- **Tombstone reclamation**: put `B`, delete `B`, merge; assert no trace of
  `B` (live or tombstone) remains in any data file, and `get(B)` still
  returns `None` afterward.
- **Untouched active file**: put some keys, note the active file's id, merge;
  assert that file id is untouched/still present and still active
  afterward.
- **Race: concurrent put during merge wins**: use a synchronization point
  (e.g. a test-only hook or a channel) to pause merge right after it reads
  `K`'s old value but before it repoints the keydir; from another thread,
  `put(K, new_value)`; let merge finish; assert `get(K) == new_value`, not
  the value merge was copying forward. This directly tests §7.3.
- **Hint files match full-scan recovery post-merge**: after a merge, reopen
  the datastore fresh (full recovery from disk, not reusing the live keydir)
  and assert the recovered keydir/data matches what was live before reopen.
- **Repeated merges are idempotent/safe**: merging an already-merged,
  unchanged datastore a second time is a no-op (or a very cheap one) and
  doesn't lose data.
- **Stress test**: one thread merging in a loop (small sleep between passes),
  several threads concurrently doing put/get/delete on random keys from a
  small keyspace (to force overlap) for a fixed duration; at the end, stop
  all threads, and assert the final state (queried via `get`) matches an
  in-memory reference `HashMap` that was updated in lock-step by the test
  (e.g. via a shared `Mutex<HashMap>` the test threads also write to
  alongside calling the real API, then compare).

---

## 8. Concurrency & locking model

### 8.1 Process-level single-writer lock

```rust
pub struct DirLock(std::fs::File);
impl DirLock {
    pub fn acquire(dir: &Path) -> Result<Self> {
        let path = dir.join(".bitcask.lock");
        let f = OpenOptions::new().create(true).write(true).open(&path)?;
        // fs2::FileExt::try_lock_exclusive, or fd-lock crate
        f.try_lock_exclusive().map_err(|_| Error::AlreadyLocked)?;
        Ok(DirLock(f))
    }
}
// Drop impl: lock releases automatically when the File is closed (OS-level
// flock semantics) — no explicit unlock needed, but document that the lock
// does NOT persist/protect against a different mechanism (e.g. NFS flock
// semantics are famously unreliable — note this as a known limitation if
// the directory might ever live on a network filesystem).
```

Add `fs2 = "0.4"` (or `fd-lock`) to `Cargo.toml`. Only acquired when
`Options::read_write` is true; read-only opens never take this lock and can
coexist with each other and with the single writer.

### 8.2 In-process synchronization summary

| Path | Lock held |
|---|---|
| `put`/`delete` (append step) | `active: Mutex<ActiveFile>` |
| `put`/`delete` (keydir update step) | `keydir: RwLock` write guard, brief |
| `get` | `keydir: RwLock` read guard (brief, just the lookup) + no lock during the actual disk `pread` (uses `DataFileSet`'s internal handle-cache mutex only briefly to fetch/insert a `File`, not for the read itself) |
| `merge` (scan phase) | no keydir lock held while reading entries off disk |
| `merge` (per-key repoint) | `keydir` write guard, one `cas_repoint` call at a time — never holds it across disk I/O |
| `fold`/`list_keys` | `keydir` read guard only for the initial snapshot, released before any disk I/O |

The guiding rule threaded through the whole design: **never hold a lock
across a disk I/O call**. Every lock acquisition above is either (a) around
a pure in-memory operation, or (b) around the specific append call that must
be serialized anyway because it's mutating shared file-offset state.

### 8.3 Tests

- Second `Bitcask::open(dir, Options{read_write: true, ..})` while the first
  is still held returns `Error::AlreadyLocked`, not a panic or hang.
- A read-only open succeeds concurrently with an existing read-write open.
- Dropping the read-write `Bitcask` (releasing the lock) allows a subsequent
  read-write open to succeed.
- Re-run the §2.4 and §7.5 threaded stress tests under `cargo test --
  --test-threads=1` with `RUST_TEST_THREADS` variations and, if available,
  under `cargo miri` or `loom` for the smaller in-memory pieces (`Keydir`
  specifically) — full loom coverage of the disk-I/O paths is likely
  impractical/unnecessary; reserve loom for `keydir.rs` only if it seems
  worthwhile once written.

---

## 9. Public API surface (mirrors the paper's Erlang API)

```rust
#[derive(Clone)]
pub struct Options {
    pub read_write: bool,
    pub sync_on_put: bool,
    pub max_file_size: u64, // default e.g. 64 MiB for a reasonable balance; paper doesn't mandate a number
}
impl Default for Options {
    fn default() -> Self {
        Self { read_write: true, sync_on_put: false, max_file_size: 64 * 1024 * 1024 }
    }
}

pub struct Bitcask { /* as in §4.1 */ }

impl Bitcask {
    pub fn open(dir: impl AsRef<Path>, opts: Options) -> Result<Self>;
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>>;
    pub fn put(&self, key: &[u8], value: &[u8]) -> Result<()>;
    pub fn delete(&self, key: &[u8]) -> Result<()>;
    pub fn list_keys(&self) -> Result<Vec<Vec<u8>>>;
    pub fn fold<A>(&self, f: impl FnMut(&[u8], &[u8], A) -> A, init: A) -> Result<A>;
    pub fn merge(&self) -> Result<()>; // operates on self, not a bare directory path — simpler for a Rust API than the paper's `bitcask:merge(DirectoryName)`, which merges a directory independent of any open handle; document the difference
    pub fn sync(&self) -> Result<()>;
    pub fn close(self) -> Result<()>; // consumes self; also just works via normal Drop if not called explicitly
}
```

Deliberate deviation from the paper worth documenting in doc comments: the
paper's `bitcask:merge(DirectoryName)` operates on a directory *independent*
of any particular open handle (any Erlang process can trigger a merge on a
Bitcask it doesn't itself have open for read/write, because BEAM processes
share the keydir via the VM). In a Rust single-process design without that
VM-level sharing, it's simpler and safer to make `merge` a method on an
already-open `&self` handle, which has direct access to the live keydir to
do the CAS-repoint safely. Note this explicitly as an intentional API
difference, not an oversight.

### 9.1 Error type

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("directory already locked for writing by another handle")]
    AlreadyLocked,
    #[error("key must not be empty")]
    EmptyKey,
    #[error("data file corrupt: {0}")]
    Corrupt(String),
    #[error("datastore is not open for writing (read-only handle)")]
    ReadOnly,
}
pub type Result<T> = std::result::Result<T, Error>;
```

Add `thiserror = "1"` to `Cargo.toml` for ergonomic error derivation (or hand
-roll `impl std::error::Error` if avoiding the dependency matters — a small
crate like this is a good place to just take the dependency).

### 9.2 Deliverables

- Finalize `src/lib.rs` re-exports (`pub use engine::Bitcask; pub use
  error::{Error, Result}; pub use engine::Options;` — keep internal modules
  private, expose only the surface above).
- Doc comments on every public method cross-referencing this plan's section
  numbers isn't necessary in the final code (don't cite this doc from
  source), just document behavior/semantics directly, matching the
  descriptions above.
- `examples/basic_usage.rs`: open a temp dir, put a few keys, get, delete,
  list_keys, fold to sum something, merge, close, reopen, verify persisted —
  a runnable, readable demonstration of the whole API in one file.

---

## 10. Testing & validation matrix

| Layer | What | Where |
|---|---|---|
| Unit | `format.rs` encode/decode/CRC/truncation | `src/format.rs` `#[cfg(test)]` |
| Unit | `keydir.rs` insert/remove/CAS + threaded stress | `src/keydir.rs` `#[cfg(test)]` |
| Unit | `datafile.rs` append/read/rotation/concurrent pread | `src/datafile.rs` `#[cfg(test)]` |
| Integration | CRUD, persistence across reopen | `tests/crud.rs` |
| Integration | Recovery: full-scan, hint-based, truncated tail, corrupt entry, orphaned hint | `tests/recovery.rs` |
| Integration | Merge: compaction, tombstone reclaim, race-safety, post-merge recovery equivalence | `tests/merge.rs` |
| Integration | Locking: second-writer rejection, reader/writer coexistence | `tests/locking.rs` |
| Integration | Concurrency stress: put/get/delete/merge all running together vs. a reference model | `tests/concurrency.rs` |
| Property (optional) | `proptest`: random op sequences (put/delete/get/reopen/merge) vs. `HashMap` reference model | `tests/model.rs` |
| Benchmark (optional) | Sequential write throughput, single-key read latency, at small scale | `benches/throughput.rs` (criterion) |

For the property-based model test, sketch the harness up front since it's
the highest-leverage test for a storage engine like this:

```rust
enum Op { Put(Key, Value), Delete(Key), Get(Key), Reopen, Merge }

fn run_model(ops: Vec<Op>) {
    let dir = tempdir();
    let mut model: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();
    let mut db = Bitcask::open(&dir, Options::default()).unwrap();
    for op in ops {
        match op {
            Op::Put(k, v) => { db.put(&k, &v).unwrap(); model.insert(k, v); }
            Op::Delete(k) => { db.delete(&k).unwrap(); model.remove(&k); }
            Op::Get(k) => assert_eq!(db.get(&k).unwrap(), model.get(&k).cloned()),
            Op::Reopen => { drop(db); db = Bitcask::open(&dir, Options::default()).unwrap(); }
            Op::Merge => db.merge().unwrap(),
        }
    }
    // final full comparison
    let mut keys: Vec<_> = db.list_keys().unwrap();
    keys.sort();
    let mut model_keys: Vec<_> = model.keys().cloned().collect();
    model_keys.sort();
    assert_eq!(keys, model_keys);
    for k in keys { assert_eq!(db.get(&k).unwrap().as_ref(), model.get(&k)); }
}
```

Run this from `proptest!` with a strategy generating short `Op` sequences
over a small key alphabet (so keys collide/overlap frequently, which is where
the interesting bugs live).

---

## 11. Suggested build order (milestones)

1. `format.rs` + CRC (§1) — no I/O yet, pure encode/decode. *Exit criterion:
   round-trip and corruption-detection unit tests pass.*
2. `keydir.rs` (§2) — pure in-memory structure. *Exit criterion: insert/
   remove/CAS unit tests + threaded stress test pass.*
3. `datafile.rs` append/read primitives, no rotation yet (§3). *Exit
   criterion: append-then-read-back test passes; concurrent pread test
   passes.*
4. Single-file engine: `put`/`get`/`delete` with **no rotation, no merge, no
   recovery** — just prove the append+keydir+read loop works in one file
   (§4, partial). *Exit criterion: §4.5 tests pass against a single fixed
   active file.*
5. Add file rotation (§3.4). *Exit criterion: small-threshold rotation test
   from §3.5 passes; §4.5 tests still pass unmodified.*
6. Add startup recovery by full scan, no hint files yet (§6.1-6.2, 6.4).
   *Exit criterion: full-scan and truncated-tail recovery tests from §6.5
   pass.*
7. Add hint files + hint-based fast recovery (§6.3). *Exit criterion:
   hint-based recovery test matches full-scan recovery test output exactly.*
8. Add `list_keys`/`fold` (§5). *Exit criterion: §5.1 tests pass.*
9. Add merge + hint file generation on merge (§7). *Exit criterion: §7.5
   tests pass, including the race test.*
10. Add locking/concurrency guarantees (§8). *Exit criterion: §8.3 tests
    pass.*
11. Polish public API, docs, examples (§9). *Exit criterion: `examples/
    basic_usage.rs` runs cleanly end to end.*
12. Fill out the remaining testing matrix (§10) — property-based model test
    and benchmarks — last, since they depend on everything above and are the
    slowest to iterate on. *Exit criterion: proptest run for a few thousand
    cases with no failures; benchmark numbers recorded as a baseline (not
    compared against the paper's numbers, which are from 2010 hardware —
    just useful as a regression baseline for this implementation going
    forward).*

Each step should land as its own commit/PR with its own tests green before
moving to the next — this keeps every intermediate state runnable, matches
the "small verifiable increments" approach the project favors, and makes it
easy to bisect if a later step reveals a bug introduced earlier.

---

## 12. Deliberate simplifications for a first pass (call out, revisit later)

- **No compression** — paper explicitly makes the same call ("very
  application-dependent").
- **No internal read cache** beyond the OS page cache — paper notes this too
  and treats it as an open question.
- **No cross-process shared keydir** — the paper's "share the keydir with
  another Erlang process in the same VM" is a BEAM-specific optimization;
  each OS process that opens the Bitcask builds and owns its own keydir.
  (A future extension could add a shared-memory keydir for multiple
  *threads* within one Rust process that already share the `Bitcask` handle,
  which this design already gets for free via `Arc<Bitcask>` — just don't
  try to share across separate OS processes.)
- **Merge holds one CAS-repoint per key, not a giant single global lock at
  the end** — already better than the paper's vague description, keep it
  this way; don't regress to a coarser lock later without a specific reason.
- **FD caching with no eviction** in `DataFileSet` — fine until proven
  otherwise by a test with an unusually large number of files; add LRU
  eviction only if that happens.
- **No compression of the hint-file/data-file format versioning** — if the
  on-disk format ever needs to change, add a version byte to the *directory*
  (e.g. a `VERSION` file) rather than to every entry, to keep the hot path
  format exactly as described in §1. Not needed for v1; noted so a future
  format change doesn't require guessing where to put the version marker.
- **No online/incremental merge** (merge is a single blocking-ish pass over
  all eligible files) — matches the paper's description; a production system
  might chunk merge work over time, but that's a real complexity jump and
  out of scope for a first implementation.
