#!/bin/sh
# Run the full guest acceptance test with a real model completion.
# Set NOCTUM_QEMU_STAGE and NOCTUM_GGUF to the test assets.
# Set NOCTUM_LLAMA_SERVER for local inference, or NOCTUM_MODEL_BRIDGE
# for native Mac inference through examples/model_probe.rs.
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
if [ -z "${NOCTUM_MODEL_BRIDGE:-}" ] && { [ -z "${NOCTUM_LLAMA_SERVER:-}" ] || [ ! -x "${NOCTUM_LLAMA_SERVER:-}" ]; }; then
    echo "Set NOCTUM_LLAMA_SERVER to the pinned llama-server binary." >&2
    exit 1
fi
cd "$ROOT"
export NOCTUM_QEMU_STAGE="$STAGE"
cargo test --locked --lib -- --ignored real_model_proposes --nocapture --test-threads=1
