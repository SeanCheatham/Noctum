#!/usr/bin/env python3
"""Stop a test build before it uses the disk reserve."""

import argparse
from contextlib import suppress
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time


GIB = 1024 ** 3


def run(command, path, sample=None, period=2):
    sample = sample or (lambda: shutil.disk_usage(path).free)
    free = sample()
    if free < 12 * GIB:
        raise RuntimeError(f"build needs 12 GiB of free space; have {free / GIB:.1f} GiB")
    process = subprocess.Popen(command, start_new_session=True)
    try:
        while process.poll() is None:
            if sample() < 2 * GIB:
                raise RuntimeError("build stopped at the 2 GiB disk reserve")
            time.sleep(period)
        return process.returncode
    finally:
        if process.poll() is None:
            with suppress(ProcessLookupError):
                os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                with suppress(ProcessLookupError):
                    os.killpg(process.pid, signal.SIGKILL)
                process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--disk", type=Path, default=Path.cwd())
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command
    if command[:1] == ["--"]:
        command = command[1:]
    if not command:
        parser.error("set a build command")
    try:
        return run(command, args.disk)
    except RuntimeError as error:
        print(error, file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
