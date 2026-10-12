//! Process-wide allocation counter. Armed only around a measured window.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static ARMED: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static FREES: AtomicU64 = AtomicU64::new(0);

pub struct CountingAlloc;

#[derive(Clone, Copy, Debug, Default)]
pub struct AllocSnapshot {
    pub allocs: u64,
    pub bytes: u64,
    pub frees: u64,
}

pub fn reset() {
    ALLOCS.store(0, Ordering::SeqCst);
    BYTES.store(0, Ordering::SeqCst);
    FREES.store(0, Ordering::SeqCst);
}

pub fn arm() {
    ARMED.store(true, Ordering::SeqCst);
}

pub fn disarm() {
    ARMED.store(false, Ordering::SeqCst);
}

pub fn snapshot() -> AllocSnapshot {
    AllocSnapshot {
        allocs: ALLOCS.load(Ordering::SeqCst),
        bytes: BYTES.load(Ordering::SeqCst),
        frees: FREES.load(Ordering::SeqCst),
    }
}

fn note(size: usize) {
    if ARMED.load(Ordering::Relaxed) {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(size as u64, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ARMED.load(Ordering::Relaxed) {
            FREES.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(new_size);
        if ARMED.load(Ordering::Relaxed) {
            FREES.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
