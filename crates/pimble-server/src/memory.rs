//! Giving freed memory back to the system (docs/DRAWING_CONTRACT.md "Large
//! canvases", "Memory"). A large canvas document (15 MB) is encoded,
//! decoded and copied several times per write; glibc's allocator keeps the
//! freed room in its heap (its dynamic mmap threshold grows past such
//! sizes), so a `pimble-mcp` holding 77 MB of live data sat at 530 MB, and
//! the app's heap kept hundreds of MB free. `malloc_trim` hands the free
//! pages back. Elsewhere (macOS, Windows, musl) this does nothing.

use std::time::Duration;

/// Hand the allocator's free pages back to the system, where that is
/// possible (glibc on Linux).
pub fn release_free_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `malloc_trim` takes no pointers and is safe to call from any
    // thread at any time.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// [`release_free_memory`] every `every`, on a thread of its own, for the
/// life of the process.
pub fn spawn_memory_trimmer(every: Duration) {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        let _ = std::thread::Builder::new().name("memory-trim".into()).spawn(move || loop {
            std::thread::sleep(every);
            release_free_memory();
        });
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    let _ = every;
}
