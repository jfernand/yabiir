//! Data file management: the single writable "active" file, and read-only
//! access to immutable (rotated-out) data files. See
//! `docs/bitcask-implementation-plan.md` §3.
//!
//! File naming: `{file_id:020}.bitcask.data` / `{file_id:020}.bitcask.hint`,
//! zero-padded decimal so directory listings sort lexicographically in
//! `file_id` order. `file_id` is `u32` throughout, per plan §3.1 — ample
//! headroom given each file is at minimum megabytes.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::format::EncodedEntry;

/// The single file currently being appended to. Tracks `offset` itself
/// (plan §3.2) rather than relying on the OS's O_APPEND cursor bookkeeping,
/// so `append`'s returned position is always correct regardless of how the
/// file was opened.
pub struct ActiveFile {
    file_id: u32,
    writer: BufWriter<File>,
    /// A second handle onto the same file, opened once alongside the
    /// writer (plan §3.3), used for positioned reads of keys that were
    /// just written to this still-active file — reads of an active file
    /// don't go through [`DataFileSet`], which only ever opens files that
    /// have already rotated out and become immutable.
    read_handle: File,
    offset: u64,
}

impl ActiveFile {
    /// Create (or resume — see the offset comment below) the active file
    /// for `file_id` in `dir`.
    pub fn create(dir: &Path, file_id: u32) -> io::Result<Self> {
        let path = DataFileSet::data_path(dir, file_id);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)?;
        // Normally 0 for a brand-new file; reading it back from the file's
        // actual length rather than assuming 0 means this would also do
        // the right thing if ever pointed at a pre-existing file.
        let offset = file.metadata()?.len();
        let read_handle = file.try_clone()?;
        Ok(Self {
            file_id,
            writer: BufWriter::new(file),
            read_handle,
            offset,
        })
    }

    pub fn file_id(&self) -> u32 {
        self.file_id
    }

    /// Append one pre-encoded entry. Returns `(file_id, value_pos,
    /// entry_total_len)` — `value_pos` is where the *value* bytes start
    /// within the file (past the header and key), which is exactly what
    /// the keydir stores and what reads seek/pread from directly.
    ///
    /// Flushes the `BufWriter` before returning (a `write` syscall, not a
    /// fsync) — necessary, not just an optimization detail: `read_at` reads
    /// through a *separate* raw file handle that bypasses this buffer, so
    /// without flushing here, a `read_at` for bytes still sitting in
    /// userspace would see a short/stale file and fail. `sync` (fsync) is
    /// still a separate, more expensive durability step, gated by
    /// `Options::sync_on_put` at the engine level.
    pub fn append(&mut self, encoded: &EncodedEntry) -> io::Result<(u32, u64, u64)> {
        let bytes = encoded.as_bytes();
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        let total_len = bytes.len() as u64;
        let value_pos = self.offset + (total_len - encoded.value_len() as u64);
        self.offset += total_len;
        Ok((self.file_id, value_pos, total_len))
    }

    /// Flush buffered writes and fsync the file's data (not metadata —
    /// `sync_data`, cheaper than `sync_all`, is enough since we only need
    /// the bytes durable, not e.g. mtime).
    pub fn sync(&mut self) -> io::Result<()> {
        self.writer.flush()?;
        self.writer.get_ref().sync_data()
    }

    /// Current end-of-file / next append position. Used by the write path
    /// to decide when to rotate (plan §3.4 — rotation itself is a
    /// write-path concern, not implemented here).
    pub fn len(&self) -> u64 {
        self.offset
    }

    pub fn is_empty(&self) -> bool {
        self.offset == 0
    }

    /// Read `len` bytes starting at `pos` from this file, without
    /// disturbing any other reader's position (positioned read, not
    /// seek-then-read) — see [`pread_exact`].
    pub fn read_at(&self, pos: u64, len: u32) -> io::Result<Vec<u8>> {
        pread_exact(&self.read_handle, pos, len)
    }
}

/// Read-only access to every data file in a Bitcask directory, active file
/// excluded (see [`ActiveFile::read_at`] for that). Read handles are opened
/// lazily on first access and cached for the life of the `DataFileSet`
/// (plan §3.3: never evict — a real Bitcask directory has at most a few
/// thousand files even at scale, thanks to merge).
pub struct DataFileSet {
    dir: PathBuf,
    readers: Mutex<HashMap<u32, Arc<File>>>,
}

impl DataFileSet {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            readers: Mutex::new(HashMap::new()),
        }
    }

    /// List every `file_id` with a `*.bitcask.data` file in `dir`, sorted
    /// ascending. Entries that don't match the naming pattern are silently
    /// skipped (forward-compatible with other files — a lockfile, metadata
    /// — later living in the same directory; plan §3.1).
    pub fn discover(dir: &Path) -> io::Result<Vec<u32>> {
        let mut ids = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if let Some(name) = entry.file_name().to_str() {
                if let Some(id) = parse_data_file_id(name) {
                    ids.push(id);
                }
            }
        }
        ids.sort_unstable();
        Ok(ids)
    }

    pub fn data_path(dir: &Path, file_id: u32) -> PathBuf {
        dir.join(format!("{file_id:020}.bitcask.data"))
    }

    pub fn hint_path(dir: &Path, file_id: u32) -> PathBuf {
        dir.join(format!("{file_id:020}.bitcask.hint"))
    }

    /// Read `len` bytes starting at `pos` from the (immutable) data file
    /// `file_id`. The handle-cache lock is held only long enough to fetch
    /// or insert the cached `Arc<File>` — never across the actual read, so
    /// concurrent reads of the same or different files never serialize on
    /// each other (plan §8.2).
    pub fn read_at(&self, file_id: u32, pos: u64, len: u32) -> io::Result<Vec<u8>> {
        let file = {
            let mut readers = self.readers.lock().unwrap();
            if let Some(f) = readers.get(&file_id) {
                Arc::clone(f)
            } else {
                let f = Arc::new(File::open(Self::data_path(&self.dir, file_id))?);
                readers.insert(file_id, Arc::clone(&f));
                f
            }
        };
        pread_exact(&file, pos, len)
    }

    /// Drop this set's cached read handle for `file_id`, if any. Called by
    /// merge after removing an input file from disk — without this, the
    /// file's disk blocks would stay allocated (kept alive by the cached
    /// open `Arc<File>`) even after `remove_file` unlinked it from the
    /// directory, for as long as this `DataFileSet` exists (plan §3.3's
    /// "never evict" was written before merge existed to ever remove
    /// files; this is the one place that assumption needs an exception).
    pub fn forget(&self, file_id: u32) {
        self.readers.lock().unwrap().remove(&file_id);
    }
}

fn parse_data_file_id(name: &str) -> Option<u32> {
    let stem = name.strip_suffix(".bitcask.data")?;
    if stem.len() != 20 || !stem.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    stem.parse().ok()
}

/// Positioned read (`pread`): reads `len` bytes starting at `pos` without
/// touching (or racing on) the file's shared seek cursor. This is the
/// single most important correctness detail for concurrent reads — using
/// `Seek` + `Read` on a handle shared across threads is a classic bug (plan
/// §3.3).
#[cfg(unix)]
fn pread_exact(file: &File, pos: u64, len: u32) -> io::Result<Vec<u8>> {
    use std::os::unix::fs::FileExt;
    let mut buf = vec![0u8; len as usize];
    file.read_exact_at(&mut buf, pos)?;
    Ok(buf)
}

#[cfg(not(unix))]
compile_error!(
    "datafile.rs currently only supports unix targets (positioned pread-based \
     reads for safe concurrent access — see plan §3.3); add a non-unix pread_exact \
     fallback before targeting another platform"
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format;
    use std::ops::Deref;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    /// Minimal self-cleaning temp directory for filesystem-backed tests, so
    /// this module doesn't need an extra dev-dependency just for that.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "yabiir-datafile-test-{}-{}-{}",
                std::process::id(),
                n,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Deref for TempDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn append_then_read_back_matches() {
        let dir = TempDir::new();
        let mut active = ActiveFile::create(&dir, 1).unwrap();

        let cases: [(&[u8], &[u8]); 3] = [(b"a", b"1"), (b"bb", b"22"), (b"ccc", b"")];
        let mut locations = Vec::new();
        for (key, value) in &cases {
            let encoded = format::encode_entry(key, value, false, 0);
            let (file_id, value_pos, _total_len) = active.append(&encoded).unwrap();
            locations.push((file_id, value_pos, value.len() as u32));
        }
        active.sync().unwrap();
        drop(active); // now immutable

        let files = DataFileSet::new(&*dir);
        for ((file_id, value_pos, value_len), (_, expected_value)) in
            locations.iter().zip(cases.iter())
        {
            let read = files.read_at(*file_id, *value_pos, *value_len).unwrap();
            assert_eq!(&read, expected_value);
        }
    }

    #[test]
    fn manual_rotation_produces_files_with_increasing_ids_and_expected_sizes() {
        let dir = TempDir::new();
        let threshold: u64 = 200;
        let mut file_id = 1u32;
        let mut active = ActiveFile::create(&dir, file_id).unwrap();
        let mut rotations = 0;

        for i in 0..50u32 {
            let key = format!("key-{i}");
            let value = vec![b'x'; 20];
            let encoded = format::encode_entry(key.as_bytes(), &value, false, 0);
            active.append(&encoded).unwrap();
            if active.len() >= threshold {
                active.sync().unwrap();
                file_id += 1;
                active = ActiveFile::create(&dir, file_id).unwrap();
                rotations += 1;
            }
        }
        active.sync().unwrap();
        drop(active);

        assert!(rotations >= 3, "expected several rotations, got {rotations}");

        let ids = DataFileSet::discover(&dir).unwrap();
        let expected: Vec<u32> = (1..=file_id).collect();
        assert_eq!(ids, expected);

        // every file except the last (still being written when the loop
        // ended) should be at or above the rotation threshold.
        for &id in &ids[..ids.len() - 1] {
            let meta = fs::metadata(DataFileSet::data_path(&dir, id)).unwrap();
            assert!(
                meta.len() >= threshold,
                "file {id} is {} bytes, expected >= {threshold}",
                meta.len()
            );
        }
    }

    #[test]
    fn concurrent_reads_on_same_file_id_are_correct() {
        let dir = TempDir::new();
        let mut active = ActiveFile::create(&dir, 1).unwrap();

        let mut locations = Vec::new();
        for i in 0..64u32 {
            let value = vec![(i % 256) as u8; 37]; // distinct-ish content per index
            let encoded = format::encode_entry(format!("k{i}").as_bytes(), &value, false, 0);
            let (file_id, value_pos, _) = active.append(&encoded).unwrap();
            locations.push((file_id, value_pos, value));
        }
        active.sync().unwrap();
        drop(active);

        let files = Arc::new(DataFileSet::new(&*dir));
        let locations = Arc::new(locations);

        let handles: Vec<_> = (0..8u32)
            .map(|t| {
                let files = Arc::clone(&files);
                let locations = Arc::clone(&locations);
                thread::spawn(move || {
                    for (idx, (file_id, value_pos, expected)) in locations.iter().enumerate() {
                        if idx as u32 % 8 != t {
                            continue; // spread work across threads, all hitting the same file_id
                        }
                        let read = files
                            .read_at(*file_id, *value_pos, expected.len() as u32)
                            .unwrap();
                        assert_eq!(&read, expected, "thread {t} idx {idx}");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }
}
