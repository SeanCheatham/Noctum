This test checks guest instruction counts on ARM64. Results are in `results.json`.

Apple runtime results are in `apple-results.json`. A full production CLI run used the native model on the pinned `base64` source. The patch passed the package test suite and both guest measurements. Its count increased from 2,484,943 to 2,488,399 instructions. Noctum rejected it and kept the source, HEAD, index, and branches unchanged. The phase records show the model with no VM, then the worker VM with no model. The final Rust workspace checks passed 117 tests. Clippy and three Python tests also passed. Earlier protocol, saved response, and failure results are kept in separate fields. These checks did not produce an accepted optimization for this project.

A saved response check also ran three passes. The first rejected the slower patch. The second skipped the unchanged function. The third used `--retry`, repeated the package tests and count checks, and rejected the slower patch again. The trial counts were 1, 0, and 1. The original tracked source and HEAD stayed unchanged. This check used no new model request.

Production CLI results are in `production-results.json`. They include four supported upstream projects, one expected build failure, and the workspace fixture checks.

Linux x86_64 results are in `native-linux-results.json`. The checks ran in Docker on an ARM64 Mac. The native Linux CLI measured the pinned `base64` test four times in two commands. Each run gave 2,617,798 instructions. The source, index, HEAD, and branches stayed unchanged. The final Rust suite passed 124 tests. The separate guest test also passed. These checks used `rust-lld`; the worker's former `musl-gcc` linker produced a test binary that failed with Rust 1.100.

The test uses memchr revision `9ba486e4ba7e865c0510305c5dacba73988d9f31` and test `memchr::tests::forward1_oneshot`. The guest has one Cortex-A72 CPU and Linux 6.12.10. Input files are in an added RAM archive. The test uses no VirtFS devices.

`build-timer.sh` builds QEMU 9.2.0 with the upstream change to `timerlist_rearm`. Run the script in the Linux worker image. Set its first argument to a mounted output directory. The script checks the source archive hash. The upstream change removes a timer advance from a timer callback. See the [QEMU change](https://gitlab.com/qemu-project/qemu/-/blob/ffbc5e661fc3b73debaec2354bf46273186bf882/util/qemu-timer.c).

Download `include/qemu/qemu-plugin.h` from QEMU tag `v9.2.0`. The example checks its SHA-256 value. Keep the new QEMU binary outside the `checkpoint-probe` directory, because the example replaces that directory.

Run the example with these values:

```sh
export NOCTUM_CHECKPOINT_RAM=1
export NOCTUM_CHECKPOINT_PRECISE=1
export NOCTUM_CHECKPOINT_SLEEP_OFF=1
export NOCTUM_CHECKPOINT_COLD=1
export NOCTUM_CHECKPOINT_HEADER=/path/to/qemu-plugin.h
export NOCTUM_CHECKPOINT_QEMU=/path/to/data/checkpoint-tools/qemu-system-aarch64-timer
cargo run --locked --example checkpoint_probe -- /path/to/data /path/to/memchr memchr::tests::forward1_oneshot
```

The QEMU path must be inside the data directory. The test uses guest seed `1` and a fixed RTC date with the VM clock. Each cold boot starts the guest again. If `NOCTUM_CHECKPOINT_COLD` is absent, the test saves the guest at the first start marker. It then restores the saved state in two new QEMU processes. The saved CPU registers must match.

The test counter has one callback per guest instruction. The old `legacy-tb-v1` counter adds a full translated block at its start. A block can stop before all its instructions execute. Keep count values from these counters separate. See the [QEMU plugin API](https://www.qemu.org/docs/master/devel/tcg-plugins.html).

With the timer change, both restores give the same count. A fixed seed also gives the same count in separate commands. Six cold boots in two commands gave 211,959,857 guest instructions each. These results cover one test and one guest target.

The former Docker worker used the RAM path and the instruction counter. It has source fixtures, package working directories, input limits, and a counter profile in run history. Its output format has lengths and a checksum and keeps arbitrary bytes. This older test tool uses text markers for output.
