# syntax=docker/dockerfile:1
FROM rust:1.100-bookworm
ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends \
    python3 python3-venv ninja-build pkg-config flex bison bc cpio \
    libelf-dev libssl-dev libglib2.0-dev libpixman-1-dev libattr1-dev \
    musl-tools libfdt-dev && rm -rf /var/lib/apt/lists/*
RUN rustup target add "$(rustc -vV | awk '/host:/ { sub(/-gnu$/, "-musl", $2); print $2 }')"
ENV CARGO_BUILD_JOBS=2
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc
ENV CC_x86_64_unknown_linux_musl=musl-gcc
ENV CC_aarch64_unknown_linux_musl=musl-gcc
WORKDIR /src
COPY qemu/ qemu/
RUN NOCTUM_GUEST_ARCH="$(uname -m)" sh qemu/build-bundle.sh && mkdir -p /opt/noctum \
    && mv qemu/out/stage /opt/noctum/guest && rm -rf qemu/src qemu/out
COPY . .
RUN --mount=type=cache,id=noctum-worker-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=noctum-worker-target-${TARGETARCH},target=/src/target \
    cargo build --release --locked --features worker --bin noctum-worker \
    && cp target/release/noctum-worker /opt/noctum/noctum-worker
RUN mkdir -m 1777 /runtime /cache
ENV CARGO_HOME=/cache/cargo
ENV HOME=/cache/home
WORKDIR /runtime
ENTRYPOINT ["/opt/noctum/noctum-worker"]
