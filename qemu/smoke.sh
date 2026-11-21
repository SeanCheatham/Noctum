#!/bin/sh
# Boot the pinned guest and print fenced instruction counts for one musl harness.
#
# Requires qemu/out/stage from qemu/build-bundle.sh. This is not part of
# `cargo test`; that suite stays free of QEMU.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
STAGE="$ROOT/qemu/out/stage"
if [ ! -x "$STAGE/qemu-system-x86_64" ]; then
    echo "qemu/out/stage is missing. Build it with: sh qemu/build-bundle.sh" >&2
    exit 1
fi
cd "$ROOT"
export NOCTUM_QEMU_STAGE="$STAGE"
cargo test --locked --lib -- --ignored fenced_musl_harness --nocapture --test-threads=1
