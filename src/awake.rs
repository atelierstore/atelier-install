//! rootshell runs a program in a web view that isn't in any window (its
//! `WasmRuntime.swift`), and WebKit can suspend such a view's work
//! (`WKPreferences.inactiveSchedulingPolicy`, which rootshell leaves as it
//! is). Runs on an iPhone behave so: what the program asks of rootshell, a
//! file read or a socket's answer, comes back through the view and wakes
//! it, but a long computation asks nothing, and stops where it is until
//! something does. Signing Atelier stopped so at 50%, hashing its 38 MB
//! binary, until Return was pressed, rootshell on screen all along
//! (12 November 2026).
//!
//! So the program checks in: from the signing's hashing
//! (`apple_codesign_quick::set_check_in`, vendor/apple-codesign-quick),
//! from the download and the install as they go, and from every 64th
//! allocation, for computations that allocate, such as a new key's. A
//! check-in asks rootshell which folder it opened for the program, which
//! goes to the app and back as a file read does and changes nothing, at
//! most every `EVERY_MS` milliseconds.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::time::{SystemTime, UNIX_EPOCH};

/// How often the program checks in at most. How long iOS waits before it
/// stops the view isn't known: on the iPhone, the signing's hashing, which
/// takes moments there, was long enough once, and not another time.
pub const EVERY_MS: u64 = 50;

/// When the program last checked in, in milliseconds since 1970.
static LAST: AtomicU64 = AtomicU64::new(0);
/// How many times it has.
static CHECKED_IN: AtomicU64 = AtomicU64::new(0);
/// How many allocations it has made.
static ALLOCATIONS: AtomicU32 = AtomicU32::new(0);

/// Checks in with rootshell, unless the program did less than `EVERY_MS`
/// ago.
pub fn check_in() {
    let now = now_ms();
    if due(LAST.load(Relaxed), now) {
        LAST.store(now, Relaxed);
        CHECKED_IN.fetch_add(1, Relaxed);
        ask();
    }
}

/// How many times the program has checked in.
pub fn checked_in() -> u64 {
    CHECKED_IN.load(Relaxed)
}

/// Whether it's time to check in again. rootshell's clocks are the
/// system's time of day, which can go back: that counts too.
fn due(last: u64, now: u64) -> bool {
    now.abs_diff(last) >= EVERY_MS
}

/// Asks rootshell what the program's first folder is.
#[cfg(target_os = "wasi")]
fn ask() {
    #[link(wasm_import_module = "wasi_snapshot_preview1")]
    unsafe extern "C" {
        fn fd_prestat_get(fd: u32, prestat: *mut u32) -> u32;
    }
    let mut prestat = [0u32; 2];
    // SAFETY: what WASI writes there, if anything, is a prestat's 8 bytes.
    unsafe { fd_prestat_get(3, prestat.as_mut_ptr()) };
}

/// Off rootshell, nothing stops the program.
#[cfg(not(target_os = "wasi"))]
fn ask() {}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// The system's allocator, checking in every 64th allocation.
pub struct Allocator;

fn allocating() {
    if ALLOCATIONS.fetch_add(1, Relaxed) % 64 == 63 {
        check_in();
    }
}

// SAFETY: it's the system's allocator; checking in allocates nothing.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocating();
        // SAFETY: as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocating();
        // SAFETY: as the caller's.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        allocating();
        // SAFETY: as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_in_every_twentieth_of_a_second() {
        assert!(due(0, 1_000));
        assert!(!due(1_000, 1_049));
        assert!(due(1_000, 1_050));
        // The clock went back.
        assert!(due(1_000, 500));
        assert!(!due(1_000, 970));
    }
}
