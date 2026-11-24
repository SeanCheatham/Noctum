//! Measurement fences for a Noctum guest run.
//!
//! Noctum adds `guard()` at the start of each measured test. It removes the
//! guard before the model reads the code and before each commit.
//!
//! On ARM64, `movz xzr, #0x4e31` starts counting and `movz xzr, #0x4e32`
//! stops counting. These instructions discard their result. They need no
//! special permission. The QEMU plugin excludes both markers from the count.

use core::arch::asm;
use core::marker::PhantomData;

/// Closes the measurement window when dropped.
///
/// Drop runs on a normal return and on unwind, so a failing assertion still
/// emits the stop fence. The guard stays on the thread that started the count.
pub struct Guard {
    _thread: PhantomData<*const ()>,
}

/// Start a measurement window. The stop fence is emitted when the guard drops.
pub fn guard() -> Guard {
    #[cfg(feature = "memory")]
    memory::start();
    fence(1);
    Guard {
        _thread: PhantomData,
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        fence(2);
        #[cfg(feature = "memory")]
        {
            memory::stop();
            memory::report();
        }
    }
}

fn fence(al: u8) {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        match al {
            1 => asm!(".inst 0xd289c63f", options(nostack, preserves_flags)),
            2 => asm!(".inst 0xd289c65f", options(nostack, preserves_flags)),
            _ => unreachable!(),
        }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = al;
        compile_error!("noctum-fence requires ARM64");
    }
}

#[cfg(feature = "memory")]
mod memory {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    static ACTIVE: AtomicBool = AtomicBool::new(false);
    static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
    static BYTES: AtomicU64 = AtomicU64::new(0);

    struct CountingAllocator;

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    fn record(pointer: *mut u8, size: usize) {
        if !pointer.is_null() && ACTIVE.load(Ordering::SeqCst) {
            ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
            BYTES.fetch_add(size as u64, Ordering::SeqCst);
        }
    }

    // Each operation delegates to System with the original pointer and layout.
    // The counters do not allocate memory.
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let pointer = System.alloc(layout);
            record(pointer, layout.size());
            pointer
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let pointer = System.alloc_zeroed(layout);
            record(pointer, layout.size());
            pointer
        }
        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            System.dealloc(pointer, layout);
        }
        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let pointer = System.realloc(pointer, layout, new_size);
            record(pointer, new_size);
            pointer
        }
    }

    pub fn start() {
        ALLOCATIONS.store(0, Ordering::SeqCst);
        BYTES.store(0, Ordering::SeqCst);
        ACTIVE.store(true, Ordering::SeqCst);
    }

    pub fn stop() {
        ACTIVE.store(false, Ordering::SeqCst);
    }

    pub fn report() {
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr().lock(),
            "\x1eNOCTUM_MEMORY_V1 {} {}",
            ALLOCATIONS.load(Ordering::SeqCst),
            BYTES.load(Ordering::SeqCst)
        );
    }
}
