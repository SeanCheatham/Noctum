#!/bin/sh
# Build the Noctum measurement bundle: a pinned qemu-system-x86_64, the TCG
# plugin, a pinned vmlinuz, busybox, the runner, and an initramfs.
#
# The daemon never measures with a distro QEMU. It only runs the directory
# this script stages, after the tarball sha256 in qemu.lock.toml has been
# checked. Running the script prints that sha256. Paste it into the lockfile.
# `noctum qemu fetch` also needs the release URL. Until that URL exists,
# `noctum qemu install` loads a local tarball whose hash matches the lockfile.
#
# Host packages (Debian/Ubuntu):
#   build-essential pkg-config python3 ninja-build flex bison bc cpio
#   libglib2.0-dev libpixman-1-dev libattr1-dev
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
need ninja
need cpio
need gzip
need find
need flex
need bison
need bc

jobs=$(nproc 2>/dev/null || echo 2)
avail_kb=$(awk '/MemAvailable/ {print $2}' /proc/meminfo 2>/dev/null || echo 0)
if [ "${avail_kb:-0}" -gt 0 ] && [ "$avail_kb" -lt 8000000 ] && [ "$jobs" -gt 2 ]; then
    jobs=2
fi
echo "building with $jobs jobs"

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

echo "compiling noctum_profile.so"
gcc -shared -fPIC -fvisibility=hidden -O2 -Wall -Wextra \
    -I"$SRC/qemu-9.2.0/include/qemu" \
    $(pkg-config --cflags glib-2.0) \
    -o "$STAGE/noctum_profile.so" \
    "$ROOT/qemu/plugin/noctum_profile.c" \
    $(pkg-config --libs glib-2.0)

if [ ! -x "$SRC/qemu-9.2.0/build/qemu-system-x86_64" ]; then
    echo "configuring and building qemu"
    (
        cd "$SRC/qemu-9.2.0"
        ./configure \
            --target-list=x86_64-softmmu \
            --enable-plugins \
            --enable-virtfs \
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
        make -j"$jobs"
    )
fi

cp "$SRC/qemu-9.2.0/build/qemu-system-x86_64" "$STAGE/qemu-system-x86_64"
cp "$SRC/qemu-9.2.0/build/qemu-img" "$STAGE/qemu-img"
chmod +x "$STAGE/qemu-system-x86_64" "$STAGE/qemu-img"
rm -rf "$STAGE/pc-bios"
mkdir -p "$STAGE/pc-bios"
find "$SRC/qemu-9.2.0/pc-bios" -type f \
    \( -name '*.bin' -o -name '*.rom' -o -name '*.img' -o -name '*.ndrv' \) \
    -exec cp {} "$STAGE/pc-bios/" \;
if [ ! -f "$STAGE/pc-bios/bios-256k.bin" ]; then
    echo "pc-bios/bios-256k.bin is missing" >&2
    exit 1
fi

if [ ! -f "$SRC/linux-6.12.10/arch/x86/boot/bzImage" ]; then
    echo "configuring and building the kernel"
    (
        cd "$SRC/linux-6.12.10"
        make tinyconfig
        ./scripts/kconfig/merge_config.sh -m .config "$ROOT/qemu/kernel.fragment"
        make olddefconfig
        for opt in \
            CONFIG_64BIT \
            CONFIG_BLK_DEV_INITRD \
            CONFIG_RD_GZIP \
            CONFIG_BINFMT_ELF \
            CONFIG_BINFMT_SCRIPT \
            CONFIG_PROC_FS \
            CONFIG_SYSFS \
            CONFIG_SHMEM \
            CONFIG_TMPFS \
            CONFIG_DEVTMPFS \
            CONFIG_SERIAL_8250 \
            CONFIG_SERIAL_8250_CONSOLE \
            CONFIG_PCI \
            CONFIG_VIRTIO_PCI \
            CONFIG_VIRTIO_BLK \
            CONFIG_NET_9P \
            CONFIG_NET_9P_VIRTIO \
            CONFIG_9P_FS \
            CONFIG_X86_IOPL_IOPERM \
            CONFIG_ACPI
        do
            if ! grep -q "^${opt}=y$" .config; then
                echo "kernel config missing $opt" >&2
                grep "$opt" .config || true
                exit 1
            fi
        done
        make -j"$jobs" bzImage
    )
fi
cp "$SRC/linux-6.12.10/arch/x86/boot/bzImage" "$STAGE/vmlinuz"

gcc -std=c11 -static -O2 -Wall -Wextra -o "$OUT/runner" "$ROOT/qemu/runner/runner.c"

BB="$SRC/busybox-1.36.1"
if [ ! -x "$BB/busybox" ]; then
    echo "building busybox"
    (
        cd "$BB"
        # allnoconfig's second pass forces every bool back to n, so a seeded
        # KCONFIG_ALLCONFIG does not survive. Merge the fragment afterwards.
        make allnoconfig
        awk '
            NR == FNR {
                if ($0 ~ /^CONFIG_[A-Za-z0-9_]+=/ || $0 ~ /^# CONFIG_[A-Za-z0-9_]+ is not set$/) {
                    key = $0
                    sub(/^# /, "", key)
                    sub(/[= ].*/, "", key)
                    force[key] = $0
                }
                next
            }
            {
                key = $0
                sub(/^# /, "", key)
                sub(/[= ].*/, "", key)
                if (key in force) next
                print
            }
            END {
                for (k in force) print force[k]
            }
        ' "$ROOT/qemu/busybox.fragment" .config > .config.merged
        mv .config.merged .config
        yes '' | make oldconfig >/dev/null
        for opt in CONFIG_STATIC CONFIG_BUSYBOX CONFIG_SH_IS_ASH CONFIG_ASH CONFIG_MOUNT CONFIG_POWEROFF; do
            if ! grep -q "^${opt}=y$" .config; then
                echo "busybox config missing $opt" >&2
                grep "$opt" .config || true
                exit 1
            fi
        done
        make -j"$jobs"
    )
fi
applets="$("$BB/busybox" --list)"
for cmd in sh mount umount mkdir echo cat poweroff; do
    if ! printf '%s\n' "$applets" | grep -qx "$cmd"; then
        echo "busybox is missing applet $cmd" >&2
        printf '%s\n' "$applets" >&2
        exit 1
    fi
done

ROOTFS="$OUT/rootfs"
rm -rf "$ROOTFS"
mkdir -p "$ROOTFS/bin" "$ROOTFS/proc" "$ROOTFS/sys" "$ROOTFS/tmp" "$ROOTFS/dev" "$ROOTFS/work" "$ROOTFS/scratch"
cp "$BB/busybox" "$ROOTFS/bin/busybox"
for cmd in sh mount umount mkdir echo cat ls poweroff reboot sleep mknod; do
    ln -sf busybox "$ROOTFS/bin/$cmd"
done
cp "$OUT/runner" "$ROOTFS/bin/runner"
cp "$ROOT/qemu/init/init" "$ROOTFS/init"
chmod +x "$ROOTFS/init" "$ROOTFS/bin/runner" "$ROOTFS/bin/busybox"
mknod -m 622 "$ROOTFS/dev/console" c 5 1 || echo "mknod console skipped" >&2
mknod -m 666 "$ROOTFS/dev/null" c 1 3 || echo "mknod null skipped" >&2

(
    cd "$ROOTFS"
    find . -print0 | cpio --null -ov --format=newc | gzip -9 > "$STAGE/initramfs.cpio.gz"
)
if [ ! -s "$STAGE/initramfs.cpio.gz" ]; then
    echo "initramfs is empty" >&2
    exit 1
fi

(
    cd "$STAGE"
    sha256sum qemu-system-x86_64 qemu-img noctum_profile.so vmlinuz initramfs.cpio.gz > MANIFEST
    find pc-bios -type f | sort | xargs sha256sum >> MANIFEST
    tar -czf "$OUT/noctum-qemu-$BUNDLE_ID.tar.gz" \
        qemu-system-x86_64 qemu-img noctum_profile.so vmlinuz initramfs.cpio.gz pc-bios MANIFEST
)

echo "bundle $OUT/noctum-qemu-$BUNDLE_ID.tar.gz"
sha256sum "$OUT/noctum-qemu-$BUNDLE_ID.tar.gz"
echo "Set qemu.lock.toml id=$BUNDLE_ID sha256=<hash above>. Leave url empty until a release hosts the tarball, and install it with noctum qemu install."
