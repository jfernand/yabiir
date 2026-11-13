//! Helpers shared by the tests and benchmarks of the yabiir crates.

use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A fresh, empty directory under the system's temporary directory, removed
/// with everything in it on drop. Dereferences to its [`Path`].
pub struct TempDir(PathBuf);

impl TempDir {
    /// Panics if the directory can't be created: this is for tests.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "yabiir-test-{}-{}-{nanos}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
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

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_temp_dir_is_distinct_and_removed_on_drop() {
        let a = TempDir::new();
        let b = TempDir::new();
        assert_ne!(&*a, &*b);
        assert!(a.is_dir());
        let path = a.to_path_buf();
        drop(a);
        assert!(!path.exists());
    }
}
