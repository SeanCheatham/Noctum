//! Measurement fences for a Noctum guest run.
//!
//! This crate is the only place the `out 0xe9` instruction is emitted. Noctum
//! injects `guard()` as the first statement of a test for a measurement build
//! and removes it before the model sees the tree and before any commit.
//!
//! The bytes of the fence are `E6 E9` (`outb %al, $0xe9`). `AL = 1` starts
//! counting and `AL = 2` stops. The QEMU plugin does not count the `out`
//! instructions themselves.

use core::arch::asm;

/// Closes the measurement window when dropped.
///
/// Drop runs on a normal return and on unwind, so a failing assertion still
/// emits the stop fence.
pub struct Guard;

/// Start a measurement window. The stop fence is emitted when the guard drops.
pub fn guard() -> Guard {
    fence(1);
    Guard
}

impl Drop for Guard {
    fn drop(&mut self) {
        fence(2);
    }
}

fn fence(al: u8) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        asm!(
            "outb %al, $0xe9",
            in("al") al,
            options(nostack, preserves_flags, att_syntax),
        );
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = al;
        compile_error!("noctum-fence only emits fences on x86_64");
    }
}
