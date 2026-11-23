#!/bin/sh
# Prepare the ARM64 Linux guest in its private root filesystem.
set -eu
export DEBIAN_FRONTEND=noninteractive
export CARGO_BUILD_JOBS=2
export CARGO_HOME=/usr/local/cargo
export CARGO_TARGET_DIR=/cache/worker-target
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld
export CC_aarch64_unknown_linux_musl=musl-gcc
apt-get update
apt-get install -y --no-install-recommends \
    python3 python3-venv ninja-build pkg-config flex bison bc cpio \
    libelf-dev libssl-dev libglib2.0-dev libpixman-1-dev libattr1-dev \
    musl-tools libfdt-dev
rm -rf /var/lib/apt/lists/*
rustup target add aarch64-unknown-linux-musl
mkdir -p /opt/noctum
rm -rf /src
mkdir -p /src
cp -R /host-source/. /src/
cd /src
guest_key=$(find qemu -type f | sort | xargs sha256sum | sha256sum)
if [ ! -f /opt/noctum/guest/source.sha256 ] || [ "$(cat /opt/noctum/guest/source.sha256)" != "$guest_key" ]; then
    sh qemu/build-bundle.sh
    rm -rf /opt/noctum/guest
    mv qemu/out/stage /opt/noctum/guest
    printf '%s' "$guest_key" > /opt/noctum/guest/source.sha256
fi
rm -rf qemu/src qemu/out
cargo build --release --locked --features worker --bin noctum-worker
cp /cache/worker-target/release/noctum-worker /opt/noctum/noctum-worker
cd /
rm -rf /src
mkdir -p /cache/cargo /cache/home /runtime
chmod 1777 /cache /runtime
