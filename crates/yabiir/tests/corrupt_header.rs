//! A data file ending in a header that claims gigabytes: opening the store
//! must recover the entries before it without allocating anything near the
//! claimed size.
//!
//! The test binary's allocator records the largest single allocation, so
//! this file holds this one test and nothing else runs alongside it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

use yabiir::datafile::DataFileSet;
use yabiir::format::encode_entry;
use yabiir::{Bitcask, Engine, Options};
use yabiir_testutil::TempDir;

struct LargestAllocation;

static LARGEST: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for LargestAllocation {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LARGEST.fetch_max(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        LARGEST.fetch_max(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LARGEST.fetch_max(new_size, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: LargestAllocation = LargestAllocation;

#[test]
fn open_recovers_past_a_header_claiming_gigabytes_without_allocating_them() {
    let dir = TempDir::new();
    let good = encode_entry(b"key", b"value", false, 1).into_bytes();
    let mut torn = encode_entry(b"next", b"value", false, 1).into_bytes();
    torn[8..12].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes()); // key_size
    torn[12..16].copy_from_slice(&u32::MAX.to_le_bytes()); // value_size
    fs::write(
        DataFileSet::data_path(&dir, 1),
        [good.as_slice(), torn.as_slice()].concat(),
    )
    .unwrap();

    LARGEST.store(0, Ordering::Relaxed);
    let db = Engine::open(&*dir, Options::default()).unwrap();
    let largest = LARGEST.load(Ordering::Relaxed);

    assert_eq!(
        db.get(b"key")
            .unwrap()
            .as_deref(),
        Some(b"value".as_slice())
    );
    assert_eq!(
        db.get(b"next")
            .unwrap(),
        None
    );
    assert!(
        largest < 16 << 20,
        "open made a {largest}-byte allocation for a {}-byte file",
        good.len() + torn.len()
    );
}
