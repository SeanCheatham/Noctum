#!/bin/sh
# One night with a real llama.cpp completion and the pinned guest.
#
# `cargo test` does not run this. The production lockfile stays pointed at
# Qwen2.5-Coder-7B-Instruct Q5_K_M. Point NOCTUM_GGUF at a coder GGUF this
# machine can load, and NOCTUM_LLAMA_SERVER at the pinned llama-server.
# The QEMU stage is qemu/out/stage from qemu/build-bundle.sh unless
# NOCTUM_QEMU_STAGE is set.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
STAGE="${NOCTUM_QEMU_STAGE:-$ROOT/qemu/out/stage}"
if [ ! -x "$STAGE/qemu-system-x86_64" ]; then
    echo "QEMU stage is missing at $STAGE. Reuse an existing bundle or run qemu/build-bundle.sh" >&2
    exit 1
fi
if [ -z "${NOCTUM_GGUF:-}" ] || [ ! -f "$NOCTUM_GGUF" ]; then
    echo "Set NOCTUM_GGUF to a coder GGUF this machine can load." >&2
    exit 1
fi
if [ -z "${NOCTUM_LLAMA_SERVER:-}" ] || [ ! -x "$NOCTUM_LLAMA_SERVER" ]; then
    echo "Set NOCTUM_LLAMA_SERVER to the pinned llama-server binary." >&2
    exit 1
fi
cd "$ROOT"
export NOCTUM_QEMU_STAGE="$STAGE"
cargo test --locked --lib -- --ignored real_model_proposes --nocapture --test-threads=1
