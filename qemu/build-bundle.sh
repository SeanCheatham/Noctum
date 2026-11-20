#!/bin/sh
# Build the Noctum measurement bundle: a pinned qemu-system-x86_64, the TCG
# plugin, a pinned vmlinuz, busybox, the runner, and an initramfs.
#
# The daemon never measures with a distro QEMU. It only runs the directory
# this script stages, after noctum qemu fetch has checked the tarball sha256
# in qemu.lock.toml. Running the script prints that sha256. Paste it into the
# lockfile with the URL the release publishes. Until then the lockfile sha256
# stays empty and fetch refuses to install an unverified bundle.
#
# Pinned sources (sha256 of the upstream archives):
#   qemu 9.2.0          f859f0bc65e1f533d040bbe8c92bcfecee5af2c921a6687c652fb44d089bd894
#   linux 6.12.10       4a516e5ed748537a73cb42ec47fbbeb6df8b1298e8892c29c0e91de79095b297
#   busybox 1.36.1      b8cc24c9574d809e7279c3be349795c5d5ceb6fdf19ca709f80cde50e47de314

set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
SRC="$ROOT/qemu/src"
OUT="$ROOT/qemu/out"
STAGE="$OUT/stage"
BUNDLE_ID="qemu-9.2.0-linux-6.12.10"

QEMU_TAR="qemu-9.2.0.tar.xz"
QEMU_URL="https://download.qemu.org/qemu-9.2.0.tar.xz"
QEMU_SHA="f859f0bc65e1f533d040bbe8c92bcfecee5af2c921a6687c652fb44d089bd894"

KERNEL_TAR="linux-6.12.10.tar.xz"
KERNEL_URL="https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.12.10.tar.xz"
KERNEL_SHA="4a516e5ed748537a73cb42ec47fbbeb6df8b1298e8892c29c0e91de79095b297"

BUSYBOX_TAR="busybox-1.36.1.tar.bz2"
BUSYBOX_URL="https://busybox.net/downloads/busybox-1.36.1.tar.bz2"
BUSYBOX_SHA="b8cc24c9574d809e7279c3be349795c5d5ceb6fdf19ca709f80cde50e47de314"

need() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "missing tool: $1" >&2
        exit 1
    fi
}

fetch() {
    url=$1
    dest=$2
    sha=$3
    if [ -f "$dest" ]; then
        got=$(sha256sum "$dest" | awk '{print $1}')
        if [ "$got" = "$sha" ]; then
            return
        fi
        rm -f "$dest"
    fi
    curl -fL --retry 3 -o "$dest.partial" "$url"
    got=$(sha256sum "$dest.partial" | awk '{print $1}')
    if [ "$got" != "$sha" ]; then
        echo "checksum mismatch for $dest" >&2
        echo "expected $sha" >&2
        echo "got      $got" >&2
        rm -f "$dest.partial"
        exit 1
    fi
    mv "$dest.partial" "$dest"
}

need curl
need sha256sum
need tar
need make
need gcc
need pkg-config
need python3
need cpio
need gzip
need find

mkdir -p "$SRC" "$STAGE"

fetch "$QEMU_URL" "$SRC/$QEMU_TAR" "$QEMU_SHA"
fetch "$KERNEL_URL" "$SRC/$KERNEL_TAR" "$KERNEL_SHA"
fetch "$BUSYBOX_URL" "$SRC/$BUSYBOX_TAR" "$BUSYBOX_SHA"

if [ ! -d "$SRC/qemu-9.2.0" ]; then
    tar -C "$SRC" -xf "$SRC/$QEMU_TAR"
fi
if [ ! -d "$SRC/linux-6.12.10" ]; then
    tar -C "$SRC" -xf "$SRC/$KERNEL_TAR"
fi
if [ ! -d "$SRC/busybox-1.36.1" ]; then
    tar -C "$SRC" -xf "$SRC/$BUSYBOX_TAR"
fi

if [ ! -x "$SRC/qemu-9.2.0/build/qemu-system-x86_64" ]; then
    (
        cd "$SRC/qemu-9.2.0"
        ./configure \
            --target-list=x86_64-softmmu \
            --enable-plugins \
            --disable-werror \
            --disable-docs \
            --disable-gtk \
            --disable-sdl \
            --disable-vnc \
            --disable-curses \
            --disable-brlapi \
            --disable-curl \
            --disable-gnutls \
            --disable-nettle \
            --disable-gcrypt \
            --disable-libssh \
            --disable-opengl \
            --disable-virglrenderer \
            --disable-capstone \
            --disable-kvm
        make -j"$(nproc)"
    )
fi

gcc -shared -fPIC -fvisibility=hidden -O2 -Wall -Wextra \
    -I"$SRC/qemu-9.2.0/include/qemu" \
    $(pkg-config --cflags glib-2.0) \
    -o "$STAGE/noctum_profile.so" \
    "$ROOT/qemu/plugin/noctum_profile.c"

cp "$SRC/qemu-9.2.0/build/qemu-system-x86_64" "$STAGE/qemu-system-x86_64"
cp "$SRC/qemu-9.2.0/build/qemu-img" "$STAGE/qemu-img"
chmod +x "$STAGE/qemu-system-x86_64" "$STAGE/qemu-img"

if [ ! -f "$SRC/linux-6.12.10/arch/x86/boot/bzImage" ]; then
    (
        cd "$SRC/linux-6.12.10"
        make defconfig
        ./scripts/config --enable CONFIG_NET_9P
        ./scripts/config --enable CONFIG_NET_9P_VIRTIO
        ./scripts/config --enable CONFIG_9P_FS
        ./scripts/config --enable CONFIG_VIRTIO
        ./scripts/config --enable CONFIG_VIRTIO_PCI
        ./scripts/config --enable CONFIG_VIRTIO_BLK
        ./scripts/config --enable CONFIG_DEVTMPFS
        ./scripts/config --enable CONFIG_TMPFS
        ./scripts/config --enable CONFIG_SERIAL_8250
        ./scripts/config --enable CONFIG_SERIAL_8250_CONSOLE
        ./scripts/config --enable CONFIG_PRINTK
        ./scripts/config --enable CONFIG_PROC_FS
        ./scripts/config --enable CONFIG_SYSFS
        ./scripts/config --enable CONFIG_BINFMT_ELF
        ./scripts/config --enable CONFIG_BLK_DEV_INITRD
        ./scripts/config --enable CONFIG_PCI
        ./scripts/config --disable CONFIG_MODULES
        make olddefconfig
        make -j"$(nproc)" bzImage
    )
fi
cp "$SRC/linux-6.12.10/arch/x86/boot/bzImage" "$STAGE/vmlinuz"

gcc -std=c11 -static -O2 -Wall -Wextra -o "$OUT/runner" "$ROOT/qemu/runner/runner.c"

BB="$SRC/busybox-1.36.1"
if [ ! -x "$BB/busybox" ]; then
    (
        cd "$BB"
        make defconfig
        sed -i 's/# CONFIG_STATIC is not set/CONFIG_STATIC=y/' .config
        make -j"$(nproc)"
    )
fi

ROOTFS="$OUT/rootfs"
rm -rf "$ROOTFS"
mkdir -p "$ROOTFS/bin" "$ROOTFS/proc" "$ROOTFS/sys" "$ROOTFS/tmp" "$ROOTFS/dev" "$ROOTFS/work" "$ROOTFS/scratch"
cp "$BB/busybox" "$ROOTFS/bin/busybox"
for cmd in sh mount umount mkdir echo cat ls poweroff reboot sleep mknod; do
    ln -sf busybox "$ROOTFS/bin/$cmd"
done
cp "$OUT/runner" "$ROOTFS/bin/runner"
cp "$ROOT/qemu/init/init" "$ROOTFS/init"
chmod +x "$ROOTFS/init"
mknod -m 622 "$ROOTFS/dev/console" c 5 1
mknod -m 666 "$ROOTFS/dev/null" c 1 3

(
    cd "$ROOTFS"
    find . -print0 | cpio --null -ov --format=newc | gzip -9 > "$STAGE/initramfs.cpio.gz"
)

(
    cd "$STAGE"
    sha256sum qemu-system-x86_64 qemu-img noctum_profile.so vmlinuz initramfs.cpio.gz > MANIFEST
    tar -czf "$OUT/noctum-qemu-$BUNDLE_ID.tar.gz" \
        qemu-system-x86_64 qemu-img noctum_profile.so vmlinuz initramfs.cpio.gz MANIFEST
)

echo "bundle $OUT/noctum-qemu-$BUNDLE_ID.tar.gz"
sha256sum "$OUT/noctum-qemu-$BUNDLE_ID.tar.gz"
echo "Set qemu.lock.toml id=$BUNDLE_ID url=<release url> sha256=<hash above>."
