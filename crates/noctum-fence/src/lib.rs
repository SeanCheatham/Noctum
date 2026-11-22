//! Measurement fences for a Noctum guest run.
//!
//! Noctum adds `guard()` at the start of each measured test. It removes the
//! guard before the model reads the code and before each commit.
//!
//! On x86_64, `outb %al, $0xe9` starts counting when AL is 1 and stops when
//! AL is 2. The calling thread needs I/O permission. The guard calls Linux
//! `ioperm` on that thread. The guest runs as root.
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
    fence(1);
    Guard {
        _thread: PhantomData,
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        fence(2);
    }
}

fn fence(al: u8) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        if al == 1 {
            permit_port();
        }
        asm!(
            "outb %al, $0xe9",
            in("al") al,
            options(nostack, preserves_flags, att_syntax),
        );
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        match al {
            1 => asm!(".inst 0xd289c63f", options(nostack, preserves_flags)),
            2 => asm!(".inst 0xd289c65f", options(nostack, preserves_flags)),
            _ => unreachable!(),
        }
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = al;
        compile_error!("noctum-fence requires x86_64 or aarch64");
    }
}

/// Linux x86_64 `ioperm` (syscall 173) for port 0xe9 on this thread.
#[cfg(target_arch = "x86_64")]
fn permit_port() {
    let rc: i64;
    unsafe {
        asm!(
            "syscall",
            in("rax") 173u64,
            in("rdi") 0xe9u64,
            in("rsi") 1u64,
            in("rdx") 1u64,
            lateout("rax") rc,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    if rc < 0 {
        eprintln!("noctum-fence: ioperm(0xe9) failed with {rc}");
        std::process::abort();
    }
}
