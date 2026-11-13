//! Data files: the one file being appended to, and read access to all of
//! them. See `docs/bitcask-implementation-plan.md` §3.
//!
//! A store is a directory of data files, `{file_id:020}.bitcask.data`, each a
//! sequence of entries (`crate::format`). Ids only grow, and for any key, the
//! file with the higher id holds the newer entry; recovery depends on that
//! order, and `crate::writer` and `crate::merge` keep it. The write path
//! appends to one file at a time, the active file ([`ActiveFile`]); a merge
//! writes its output files the same way. Every other file is immutable until
//! a merge removes it. A file written by a merge has a companion
//! `{file_id:020}.bitcask.hint`.
//!
//! - [`ActiveFile`]: appends entries to one file and tracks where each landed.
//! - [`DataFileSet`]: a cached read handle per file, for positioned reads.
//! - `scan`: every entry of one file, in order, for recovery and merge.

mod active;
mod name;
mod pread;
mod scan;
mod set;

pub use active::ActiveFile;
pub(crate) use pread::pread_exact;
pub(crate) use scan::{Scanned, scan};
pub use set::DataFileSet;

#[cfg(test)]
mod tests;
