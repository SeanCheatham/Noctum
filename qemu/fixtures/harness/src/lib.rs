//! Static musl harness booted by `qemu/smoke.sh`.
//!
//! `guard` executes `out 0xe9`. That instruction is not valid on the host.
//! Build this package with `cargo test --no-run` and run it only in the guest.

/// Fixed work. `black_box` keeps the loop in the binary the guest counts.
pub fn burn(iterations: u64) -> u64 {
    let mut x = 0x1234_5678_9abc_def0u64;
    for _ in 0..iterations {
        x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
    }
    x
}

#[cfg(test)]
mod tests {
    use super::burn;

    #[test]
    fn small_loop() {
        let _guard = noctum_fence::guard();
        assert_ne!(burn(1_000), 0);
    }

    #[test]
    fn encode_batch() {
        let _guard = noctum_fence::guard();
        assert_ne!(burn(10_000), 0);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn fence_bytes_in_an_immediate_are_not_a_marker() {
        let _guard = noctum_fence::guard();
        unsafe {
            std::arch::asm!("mov eax, 1", "mov ecx, 0xe9e6", out("eax") _, out("ecx") _);
        }
        assert_ne!(burn(1_000), 0);
    }
}
