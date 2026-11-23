"""Check the Swift helper with a real worker VM."""

import argparse
import json
import os
import subprocess
import tempfile
import time
from pathlib import Path

IMAGE = "docker.io/library/alpine@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f"
RAW = [0, 255, 13, 10, 128]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--helper", type=Path, required=True)
    parser.add_argument("--kernel", type=Path, required=True)
    parser.add_argument("--data", type=Path, required=True)
    parser.add_argument("--network", action="store_true")
    args = parser.parse_args()
    args.data.mkdir(parents=True, exist_ok=True)
    data = args.data.resolve()
    with tempfile.TemporaryDirectory(prefix="shared, files ", dir=data) as folder:
        shared = Path(folder)
        control = shared / "control"
        control.mkdir()
        source = shared / "input.bin"
        source.write_bytes(bytes(RAW))
        with (data / "helper.stderr").open("w") as errors:
            process = subprocess.Popen(
                [str(args.helper.resolve(strict=True))],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=errors,
                text=True,
                env={**os.environ, "PATH": "/usr/bin:/bin"},
            )

            def call(request):
                process.stdin.write(json.dumps(request) + "\n")
                process.stdin.flush()
                line = process.stdout.readline()
                if not line:
                    raise RuntimeError("The helper closed its response pipe.")
                result = json.loads(line)
                if result["error"] is not None:
                    raise RuntimeError(result["error"])
                return result["value"]

            try:
                assert call({"operation": "hello"})["protocolVersion"] == 1
                start = {
                    "operation": "start",
                    "store": str(data / "store"),
                    "kernel": str(args.kernel.resolve(strict=True)),
                    "image": IMAGE,
                    "memoryMb": 512,
                    "rootfsSizeMb": 512,
                    "network": args.network,
                    "mounts": [
                        {"source": str(shared), "destination": "/input", "readOnly": True},
                        {"source": str(control), "destination": "/control", "readOnly": False},
                    ],
                }
                first = call(start)
                result = call({"operation": "exec", "arguments": ["/bin/sh", "-c", 'printf %s "$NOCTUM_WORKER_SESSION"'], "timeoutSeconds": 20})
                assert bytes(result["stdout"]).decode() == first["id"]
                result = call({"operation": "exec", "arguments": ["/bin/sh", "-c", "cat /input/input.bin; cat >&2; exit 7"], "input": RAW, "timeoutSeconds": 20})
                assert result == {"code": 7, "stdout": RAW, "stderr": RAW}, result
                result = call({"operation": "exec", "arguments": ["/bin/sh", "-c", "echo changed > /input/input.bin"], "timeoutSeconds": 20})
                assert result["code"] != 0 and source.read_bytes() == bytes(RAW)
                if args.network:
                    result = call({"operation": "exec", "arguments": ["/bin/sh", "-c", "wget -qO- http://example.com | head -c 80"], "timeoutSeconds": 30})
                    assert result["code"] == 0 and result["stdout"], result
                call({"operation": "stop"})
                call({"operation": "stop"})
                source.write_bytes(bytes(RAW * 2))
                second = call(start)
                assert first["id"] != second["id"]
                result = call({"operation": "exec", "arguments": ["cat", "/input/input.bin"], "timeoutSeconds": 20})
                assert result["stdout"] == RAW * 2, result
                process.stdin.write(json.dumps({"operation": "exec", "arguments": ["/bin/sh", "-c", "touch /control/started; sleep 60"], "timeoutSeconds": 90}) + "\n")
                process.stdin.flush()
                deadline = time.monotonic() + 20
                while not (control / "started").exists():
                    assert time.monotonic() < deadline, "The guest command did not start."
                    time.sleep(0.02)
                stopped = time.monotonic()
                process.stdin.close()
                process.stdout.close()
                assert process.wait(timeout=25) == 0
                elapsed = time.monotonic() - stopped
                result = {"passed": True, "binary_io": RAW, "exit_code": 7, "read_only_share": True, "guest_network": args.network, "source_growth_after_restart": True, "new_session_id_after_restart": True, "repeated_stop": True, "pipe_closed_during_command": True, "stop_seconds": elapsed}
                (data / "result.json").write_text(json.dumps(result, indent=2) + "\n")
                print(json.dumps(result, indent=2))
            finally:
                if process.poll() is None:
                    process.stdin.close()
                    try:
                        process.wait(timeout=25)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()


if __name__ == "__main__":
    main()
