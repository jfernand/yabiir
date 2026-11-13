//! How a store's files are named, and which ones a directory holds.
//!
//! The id is zero-padded to 20 digits so that a directory listing sorts in
//! id order.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

const DATA_SUFFIX: &str = ".bitcask.data";
const HINT_SUFFIX: &str = ".bitcask.hint";
const ID_DIGITS: usize = 20;

pub(super) fn data_path(dir: &Path, file_id: u32) -> PathBuf {
    dir.join(format!("{file_id:0ID_DIGITS$}{DATA_SUFFIX}"))
}

pub(super) fn hint_path(dir: &Path, file_id: u32) -> PathBuf {
    dir.join(format!("{file_id:0ID_DIGITS$}{HINT_SUFFIX}"))
}

/// The id in a data file's name, or `None` for any other name.
pub(super) fn parse_data_file_id(name: &str) -> Option<u32> {
    let stem = name.strip_suffix(DATA_SUFFIX)?;
    let is_an_id = stem.len() == ID_DIGITS
        && stem
            .bytes()
            .all(|b| b.is_ascii_digit());
    if !is_an_id {
        return None;
    }
    stem.parse()
        .ok()
}

/// The id of every data file in `dir`, ascending. Anything else in the
/// directory (hint files, the lock file, files that aren't the store's) is
/// skipped.
pub(super) fn discover(dir: &Path) -> io::Result<Vec<u32>> {
    let mut ids = Vec::new();
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        if let Some(id) = name
            .to_str()
            .and_then(parse_data_file_id)
        {
            ids.push(id);
        }
    }
    ids.sort_unstable();
    Ok(ids)
}

/// Make the directory's current list of files durable: fsync the directory
/// itself. Syncing a file makes its bytes durable but not its name; without
/// this, a power failure can leave a newly created file missing, or a removed
/// one still there.
pub(super) fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}
