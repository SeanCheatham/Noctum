#!/bin/sh
# Run Linux guest checks with native Mac inference.
# Usage: sh experiments/inkling/acceptance.sh DATA_DIRECTORY NEW_OUTPUT_DIRECTORY
set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
DATA=${1:?Set the model data directory}
OUTPUT=${2:?Set a new output directory}
mkdir -p "$DATA" "$(dirname "$OUTPUT")"
mkdir "$OUTPUT"
DATA=$(CDPATH= cd -- "$DATA" && pwd)
OUTPUT=$(CDPATH= cd -- "$OUTPUT" && pwd)
CONTAINER="noctum-inkling-acceptance-$$"
cd "$ROOT"

cargo build --release --locked --bin noctum --example model_probe
python3 -c 'import json,sys; print("data_dir = " + json.dumps(sys.argv[1], ensure_ascii=False))' "$DATA" > "$OUTPUT/config.toml"
target/release/noctum --config "$OUTPUT/config.toml" model pull > "$OUTPUT/model-pull.log" 2>&1
docker build -t noctum-inkling-check -f experiments/inkling/Dockerfile experiments/inkling > "$OUTPUT/docker-build.log" 2>&1

docker_stage() {
    docker run --rm --name "$CONTAINER" \
        -v "$ROOT:/work" \
        -v noctum-inkling-qemu-src:/work/qemu/src \
        -v noctum-inkling-qemu-out:/work/qemu/out \
        -v noctum-inkling-linux-target:/work/target \
        -v "$OUTPUT:/bridge" \
        -v "$DATA/models/Inkling-2-Mini-UD-Q2_K_XL.gguf:/weights/model.gguf:ro" \
        -e GUEST_CROSS_COMPILE=x86_64-linux-gnu- \
        -e NOCTUM_QEMU_STAGE=/work/qemu/out/stage \
        -e NOCTUM_MODEL_BRIDGE=/bridge \
        -e NOCTUM_GGUF=/weights/model.gguf \
        noctum-inkling-check "$@"
}

docker_stage sh qemu/build-bundle.sh > "$OUTPUT/guest-build.log" 2>&1
target/release/examples/model_probe bridge "$DATA" "$OUTPUT" > "$OUTPUT/native-model.log" 2>&1 &
WORKER=$!
cleanup() {
    touch "$OUTPUT/stop"
    docker stop --timeout 1 "$CONTAINER" >/dev/null 2>&1 || true
    kill -INT "$WORKER" 2>/dev/null || true
    wait "$WORKER" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
docker_stage sh -c 'cargo test --locked -p noctum && sh qemu/smoke.sh && sh qemu/real-pass.sh' > "$OUTPUT/guest-tests.log" 2>&1
