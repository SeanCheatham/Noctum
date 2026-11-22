//! The guest instruction set is fixed for each run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GuestTarget {
    #[default]
    X86_64,
    Aarch64,
}
impl GuestTarget {
    pub fn native() -> Self {
        if cfg!(target_arch = "aarch64") {
            Self::Aarch64
        } else {
            Self::X86_64
        }
    }
    pub fn triple(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64-unknown-linux-musl",
            Self::Aarch64 => "aarch64-unknown-linux-musl",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }
    pub fn binary(self) -> &'static str {
        match self {
            Self::X86_64 => "qemu-system-x86_64",
            Self::Aarch64 => "qemu-system-aarch64",
        }
    }
    pub fn machine(self) -> &'static str {
        match self {
            Self::X86_64 => "pc",
            Self::Aarch64 => "virt,gic-version=2",
        }
    }
    pub fn cpu(self) -> &'static str {
        match self {
            Self::X86_64 => "qemu64",
            Self::Aarch64 => "cortex-a72",
        }
    }
    pub fn console(self) -> &'static str {
        match self {
            Self::X86_64 => "console=ttyS0,115200 nokaslr panic=1 loglevel=3 earlyprintk=serial",
            Self::Aarch64 => "console=ttyAMA0,115200 nokaslr panic=1 loglevel=3",
        }
    }
    pub fn icount(self) -> &'static str {
        match self {
            Self::X86_64 => "shift=0,sleep=off",
            // QEMU 9.2 needs clock progress during ARM64 idle instructions.
            Self::Aarch64 => "shift=0,sleep=on",
        }
    }
}
