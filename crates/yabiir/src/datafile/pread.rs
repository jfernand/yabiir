//! Positioned reads.

use std::fs::File;
use std::io;

/// Read exactly `len` bytes of `file` starting at `pos`, with `pread`: the
/// position is an argument, not the handle's cursor, so any number of threads
/// can read through one shared handle at once. Seek-then-read on a shared
/// handle would race.
#[cfg(unix)]
pub(crate) fn pread_exact(file: &File, pos: u64, len: u32) -> io::Result<Vec<u8>> {
    use std::os::unix::fs::FileExt;
    let mut buf = vec![0u8; len as usize];
    file.read_exact_at(&mut buf, pos)?;
    Ok(buf)
}

#[cfg(not(unix))]
compile_error!(
    "yabiir reads values with pread, which this target doesn't have; add a positioned-read \
     fallback to datafile/pread.rs before building for it"
);
