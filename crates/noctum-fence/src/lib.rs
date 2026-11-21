//! Measurement fences for a Noctum guest run.
//!
//! This crate is the only place the `out 0xe9` instruction is emitted. Noctum
//! injects `guard()` as the first statement of a test for a measurement build
//! and removes it before the model sees the tree and before any commit.
//!
//! The bytes of the fence are `E6 E9` (`outb %al, $0xe9`). `AL = 1` starts
//! counting and `AL = 2` stops. The QEMU plugin does not count the `out`
//! instructions themselves.
//!
//! `out` is privileged. libtest runs the body on a worker thread, and that
//! thread does not inherit an I/O bitmap granted before `exec`. `guard` issues
//! `ioperm` on the calling thread before the start fence. The guest is root,
//! which is what that syscall requires.

use core::arch::asm;
use core::marker::PhantomData;

/// Closes the measurement window when dropped.
///
/// Drop runs on a normal return and on unwind, so a failing assertion still
/// emits the stop fence. The guard stays on the thread that opened the port.
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
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = al;
        compile_error!("noctum-fence only emits fences on x86_64");
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
