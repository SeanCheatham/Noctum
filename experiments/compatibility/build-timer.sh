#!/bin/sh
# Build QEMU 9.2.0 with the upstream timer change for a test.
set -eu
output=$1
source_dir=$(mktemp -d)
trap 'rm -rf "$source_dir"' EXIT
cd "$source_dir"
curl -fL --retry 3 -o qemu.tar.xz https://download.qemu.org/qemu-9.2.0.tar.xz
echo 'f859f0bc65e1f533d040bbe8c92bcfecee5af2c921a6687c652fb44d089bd894  qemu.tar.xz' | sha256sum -c -
tar -xf qemu.tar.xz
cd qemu-9.2.0
python3 - <<'PY'
from pathlib import Path
p = Path('util/qemu-timer.c')
source = p.read_text()
old = '''    /* Interrupt execution to force deadline recalculation.  */
    if (icount_enabled() && timer_list->clock->type == QEMU_CLOCK_VIRTUAL) {
        icount_start_warp_timer();
    }
'''
assert source.count(old) == 1
p.write_text(source.replace(old, ''))
PY
./configure --target-list=aarch64-softmmu --enable-plugins --enable-virtfs \
    --disable-werror --disable-docs --disable-gtk --disable-sdl --disable-vnc \
    --disable-curses --disable-brlapi --disable-curl --disable-gnutls \
    --disable-nettle --disable-gcrypt --disable-libssh --disable-opengl \
    --disable-virglrenderer --disable-capstone --disable-kvm
make -j2 qemu-system-aarch64
cp build/qemu-system-aarch64 "$output/qemu-system-aarch64-timer"
sha256sum "$output/qemu-system-aarch64-timer"
