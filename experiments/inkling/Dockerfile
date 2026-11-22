FROM rust:1.100-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends \
    python3 python3-venv ninja-build pkg-config flex bison bc cpio \
    libelf-dev libssl-dev libglib2.0-dev libpixman-1-dev libattr1-dev \
    gcc-x86-64-linux-gnu qemu-user \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-linux-musl
RUN apt-get update && apt-get install -y --no-install-recommends libc6-dev-amd64-cross \
    && rm -rf /var/lib/apt/lists/*
RUN sed -i 's|/usr/lib/x86_64-linux-gnu/|/usr/x86_64-linux-gnu/lib/|g' /usr/x86_64-linux-gnu/lib/libm.a
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-gnu-gcc
ENV CARGO_BUILD_JOBS=2
WORKDIR /work
