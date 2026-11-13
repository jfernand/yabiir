//! The engine's tests, by subject.

mod basics;
mod handles;
mod metrics;
mod races;
mod reopen;
mod scan;
mod sync_on_put;

use std::path::Path;

use super::Engine;
use crate::testutil::TempDir;
use crate::{Bitcask, Error, Options, now_unix};

fn open(dir: &Path) -> Engine {
    Engine::open(dir, Options::default()).unwrap()
}
