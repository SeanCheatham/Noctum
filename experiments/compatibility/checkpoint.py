"""Test a QEMU checkpoint at an ARM64 measurement marker."""

import json
import gzip
import hashlib
import os
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time


source = Path(sys.argv[1])
harness = Path(sys.argv[2])
bundle = Path("/opt/noctum/guest")
scratch = Path(tempfile.mkdtemp(prefix="checkpoint-", dir="/runtime/scratch"))
trace = source / "checkpoint.trace"
prefixes = {}
ram = os.environ.get("NOCTUM_CHECKPOINT_RAM") == "1"
precise = os.environ.get("NOCTUM_CHECKPOINT_PRECISE") == "1"
sleep_off = os.environ.get("NOCTUM_CHECKPOINT_SLEEP_OFF") == "1"
cold = os.environ.get("NOCTUM_CHECKPOINT_COLD") == "1"
qemu_binary = Path(os.environ.get("NOCTUM_CHECKPOINT_QEMU", bundle / "qemu-system-aarch64"))
seed = os.environ.get("NOCTUM_CHECKPOINT_SEED", "1")
sample = 0


def note(message):
    with trace.open("a") as stream:
        stream.write(message + "\n")
    print(message, flush=True)


def connect(path):
    deadline = time.monotonic() + 30
    while True:
        sock = socket.socket(socket.AF_UNIX)
        sock.settimeout(90)
        try:
            sock.connect(str(path))
            return sock
        except (FileNotFoundError, ConnectionRefusedError):
            sock.close()
            if time.monotonic() > deadline:
                raise
            time.sleep(0.05)


def serial_connection():
    stream = connect(scratch / "serial.sock")
    def drain():
        try:
            while stream.recv(65536):
                pass
        except OSError:
            pass
    threading.Thread(target=drain, daemon=True).start()
    return stream


def wait_exit(process, qmp, gdb=None):
    process.wait(timeout=120)


class Qmp:
    def __init__(self, path):
        self.socket = connect(path)
        self.stream = self.socket.makefile("rwb", buffering=0)
        self.stream.readline()
        self.command("qmp_capabilities")

    def command(self, name, **arguments):
        request = {"execute": name}
        if arguments:
            request["arguments"] = arguments
        self.stream.write(json.dumps(request).encode() + b"\n")
        while True:
            response = json.loads(self.stream.readline())
            if "error" in response:
                raise RuntimeError(response)
            if "return" in response:
                return response["return"]


class Gdb:
    def __init__(self, path):
        self.socket = connect(path)

    def packet(self):
        while True:
            byte = self.socket.recv(1)
            if not byte:
                raise RuntimeError("GDB closed")
            if byte == b"$":
                break
        body = bytearray()
        while True:
            byte = self.socket.recv(1)
            if byte == b"#":
                break
            if not byte:
                raise RuntimeError("GDB closed")
            body.extend(byte)
        checksum = self.socket.recv(2)
        if int(checksum, 16) != sum(body) % 256:
            raise RuntimeError("GDB checksum failed")
        self.socket.sendall(b"+")
        return body.decode()

    def command(self, text, wait=True):
        body = text.encode()
        self.socket.sendall(b"$" + body + b"#" + f"{sum(body) % 256:02x}".encode())
        ack = self.socket.recv(1)
        if ack != b"+":
            raise RuntimeError(f"GDB acknowledgement: {ack!r}")
        return self.packet() if wait else None


def marker_addresses():
    data = harness.read_bytes()
    if data[4:6] != b"\x02\x01" or struct.unpack_from("<H", data, 18)[0] != 183:
        raise RuntimeError("the probe requires an ARM64 ELF64 executable")
    if struct.unpack_from("<H", data, 16)[0] != 2:
        raise RuntimeError("the probe requires a fixed executable load address")
    offset = struct.unpack_from("<Q", data, 32)[0]
    stride, count = struct.unpack_from("<HH", data, 54)
    addresses = []
    for index in range(count):
        kind, flags, file_offset, address, _, size, _, _ = struct.unpack_from("<IIQQQQQQ", data, offset + stride * index)
        if kind != 1 or not flags & 1:
            continue
        code = data[file_offset:file_offset + size]
        for position in range(0, len(code) - 3, 4):
            if code[position:position + 4] == bytes.fromhex("3fc689d2"):
                addresses.append(address + position)
    if not addresses:
        raise RuntimeError("start markers are missing")
    return addresses


def ram_initrd(arguments):
    init = b'''#!/bin/sh
export PATH=/bin:/sbin
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev
mount -t tmpfs tmpfs /tmp
echo 0 > /proc/sys/kernel/randomize_va_space
exec </dev/console >/dev/console 2>&1
mount -o bind /work /work
mount -o remount,bind,ro /work
cd /work
/bin/runner /scratch/job /scratch
echo NOCTUM_STDOUT_BEGIN
cat /scratch/stdout
echo NOCTUM_STDOUT_END
echo NOCTUM_MAP_BEGIN
cat /scratch/map
echo NOCTUM_MAP_END
echo NOCTUM_EXIT_BEGIN
cat /scratch/exit
echo NOCTUM_EXIT_END
poweroff -f
'''
    entries = [("init", 0o100755, init), ("work", 0o040755, b""), ("scratch", 0o040755, b""),
               ("work/probe", 0o100755, harness.read_bytes()),
               ("scratch/job", 0o100644, ("\n".join(["/work/probe", *arguments]) + "\n").encode()),
               ("TRAILER!!!", 0, b"")]
    archive = bytearray()
    for index, (name, mode, contents) in enumerate(entries, 1):
        name = name.encode() + b"\0"
        fields = [index, mode, 0, 0, 1, 0, len(contents), 0, 0, 0, 0, len(name), 0]
        archive.extend(b"070701" + "".join(f"{field:08x}" for field in fields).encode() + name)
        archive.extend(b"\0" * (-len(archive) % 4))
        archive.extend(contents)
        archive.extend(b"\0" * (-len(archive) % 4))
    path = scratch / "input.cpio.gz"
    path.write_bytes((bundle / "initramfs.cpio.gz").read_bytes() + gzip.compress(archive, mtime=0))
    return path


def launch(snapshot):
    for name in ["gdb.sock", "qmp.sock", "serial.sock", "dump.txt", "serial.log"]:
        (scratch / name).unlink(missing_ok=True)
    for name in ["stdout", "stderr"]:
        path = scratch / name
        if path.exists():
            with path.open("r+b") as stream:
                prefix = prefixes.get(name, b"")
                stream.write(prefix)
                stream.truncate(len(prefix))
    args = [
        str(qemu_binary), "-accel", "tcg", "-machine", "virt,gic-version=2",
        "-cpu", "cortex-a72", "-smp", "1", "-m", "512", "-display", "none", "-L", str(bundle / "pc-bios"),
        "-nic", "none", "-seed", seed, "-rtc", "base=2000-01-01T00:00:00,clock=vm", "-icount", "shift=0,sleep=off" if sleep_off else "shift=0,sleep=on", "-no-reboot",
        "-kernel", str(bundle / "vmlinuz"), "-initrd", str(scratch / "input.cpio.gz") if ram else str(bundle / "initramfs.cpio.gz"),
        "-append", os.environ.get("NOCTUM_CHECKPOINT_CMDLINE", "console=ttyAMA0,115200 nokaslr panic=1 loglevel=3"),
        "-drive", f"if=none,id=disk,file={scratch / 'disk.qcow2'},format=qcow2", "-device", "virtio-blk-pci,drive=disk",
        "-fsdev", f"local,id=work,path={source},security_model=none,readonly=on", "-device", "virtio-9p-pci,fsdev=work,mount_tag=work",
        "-fsdev", f"local,id=scratch,path={scratch},security_model=none", "-device", "virtio-9p-pci,fsdev=scratch,mount_tag=scratch",
        "-chardev", f"socket,id=ser,path={scratch / 'serial.sock'},server=on,wait=off,logfile={scratch / 'serial.log'}", "-serial", "chardev:ser",
        "-qmp", f"unix:{scratch / 'qmp.sock'},server=on,wait=off", "-gdb", f"unix:{scratch / 'gdb.sock'},server=on,wait=off",
        "-plugin", f"{source / 'profile-insn.so' if precise else bundle / 'noctum_profile.so'},output={scratch / 'dump.txt'}", "-S",
    ]
    if snapshot:
        args.extend(["-loadvm", snapshot])
    if ram:
        for index in range(len(args) - 1, -1, -1):
            if args[index] in ("-fsdev", "-device") and (args[index] == "-fsdev" or args[index + 1].startswith("virtio-9p")):
                del args[index:index + 2]
    error = (source / "checkpoint-qemu.stderr").open("ab")
    process = subprocess.Popen(args, stdout=error, stderr=error)
    return process


def count():
    global sample
    shutil.copyfile(scratch / "dump.txt", source / f"checkpoint-{sample}.dump")
    sample += 1
    lines = (scratch / "dump.txt").read_text().splitlines()
    errors = [line for line in lines if line.startswith("error ")]
    if errors:
        raise RuntimeError(errors)
    if ram:
        serial = (scratch / "serial.log").read_text().replace("\r", "")
        for kind, name in [("STDOUT", "stdout"), ("MAP", "map"), ("EXIT", "exit")]:
            body = serial.split(f"NOCTUM_{kind}_BEGIN\n", 1)[1].split(f"NOCTUM_{kind}_END", 1)[0]
            (scratch / name).write_text(body)
    mapping = dict(line.split() for line in (scratch / "map").read_text().splitlines())
    start, end = int(mapping["start"], 16), int(mapping["end"], 16)
    total = int(next(line.removeprefix("total ") for line in lines if line.startswith("total ")))
    application = sum(int(parts[2]) for line in lines if (parts := line.split())[0] == "bb" and start <= int(parts[1], 16) < end)
    output = (scratch / "stdout").read_text()
    output = "\n".join(line.split("finished in ")[0] if "test result:" in line else line for line in output.splitlines())
    return {"guest": total, "application": application, "stdout": output, "exit": int((scratch / "exit").read_text())}


def finish(counts):
    compared = counts if cold else counts[1:]
    result = {"transport": "initramfs" if ram else "virtfs", "instruction_callbacks": precise, "sleep_off": sleep_off, "seed": seed,
              "mode": "cold-boot" if cold else "checkpoint",
              "qemu_sha256": hashlib.sha256(qemu_binary.read_bytes()).hexdigest(),
              "harness_sha256": hashlib.sha256(harness.read_bytes()).hexdigest(),
              "counts": counts, "repeatable": len({(c["guest"], c["application"], c["stdout"], c["exit"]) for c in compared}) == 1 and all(c["exit"] == 0 for c in counts)}
    (source / "checkpoint-result.json").write_text(json.dumps(result, indent=2) + "\n")
    if not result["repeatable"]:
        raise RuntimeError("instruction counts did not repeat")


process = None
try:
    trace.write_text("")
    (source / "checkpoint-result.json").unlink(missing_ok=True)
    addresses = marker_addresses()
    note(f"Start markers: {[hex(a) for a in addresses]}")
    shutil.copyfile("/runtime/snapshots/ready.qcow2", scratch / "disk.qcow2")
    guest_executable = "/work/" + str(harness.relative_to(source))
    arguments = json.loads((source / "checkpoint-args.json").read_text())
    if ram:
        ram_initrd(arguments)
    (scratch / "job").write_text("\n".join([guest_executable, *arguments]) + "\n")
    if cold:
        if not ram:
            raise RuntimeError("cold boots require the RAM image")
        counts = []
        for index in range(3):
            process = launch(None)
            qmp = Qmp(scratch / "qmp.sock")
            serial = serial_connection()
            qmp.command("cont")
            wait_exit(process, qmp)
            counts.append(count())
            note(f"Cold boot {index + 1} count: {counts[-1]}")
        finish(counts)
        sys.exit(0)
    process = launch(None if ram else "ready")
    qmp = Qmp(scratch / "qmp.sock")
    gdb = Gdb(scratch / "gdb.sock")
    serial = serial_connection()
    for address in addresses:
        reply = gdb.command(f"Z0,{address:x},4")
        if reply != "OK":
            raise RuntimeError(f"breakpoint failed: {reply}")
    if not ram:
        serial.sendall(b"go\n")
    for attempt in range(128):
        stopped = gdb.command("c")
        pc = int.from_bytes(bytes.fromhex(gdb.command("p20")), "little")
        if pc in addresses and gdb.command(f"m{pc:x},4") == "3fc689d2":
            break
        note(f"Ignore another process at {pc:#x}")
        for address in addresses:
            gdb.command(f"z0,{address:x},4")
        gdb.command("s")
        for address in addresses:
            gdb.command(f"Z0,{address:x},4")
    else:
        raise RuntimeError("the selected process did not reach its start marker")
    note(f"Stopped at marker: {stopped}, PC {pc:#x}")
    registers = gdb.command("g")
    note(f"Checkpoint registers: {hashlib.sha256(registers.encode()).hexdigest()}")
    if not ram:
        prefixes = {name: (scratch / name).read_bytes() for name in ["stdout", "stderr"]}
    for address in addresses:
        gdb.command(f"z0,{address:x},4")
    saved = qmp.command("human-monitor-command", **{"command-line": "savevm before-test"})
    note(f"Checkpoint result: {saved!r}")
    if saved:
        raise RuntimeError(f"checkpoint was not saved: {saved}")
    gdb.command("c", wait=False)
    wait_exit(process, qmp, gdb)
    counts = [count()]
    note(f"Initial count: {counts[-1]}")
    shutil.copyfile(scratch / "disk.qcow2", scratch / "checkpoint.qcow2")
    for index in range(2):
        shutil.copyfile(scratch / "checkpoint.qcow2", scratch / "disk.qcow2")
        process = launch("before-test")
        qmp = Qmp(scratch / "qmp.sock")
        serial = serial_connection()
        replay_gdb = Gdb(scratch / "gdb.sock")
        replay_registers = replay_gdb.command("g")
        note(f"Replay {index + 1} registers: {hashlib.sha256(replay_registers.encode()).hexdigest()}")
        if replay_registers != registers:
            raise RuntimeError("CPU registers changed on checkpoint restore")
        qmp.command("cont")
        wait_exit(process, qmp)
        counts.append(count())
        note(f"Replay {index + 1} count: {counts[-1]}")
    finish(counts)
finally:
    if process is not None and process.poll() is None:
        process.kill()
        process.wait()
    if (scratch / "serial.log").exists():
        shutil.copyfile(scratch / "serial.log", source / "checkpoint-serial.log")
    shutil.rmtree(scratch)
