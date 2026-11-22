#!/usr/bin/env python3
"""Compare local models with a Noctum prompt and patch checks."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
FIXTURE = ROOT / "qemu/fixtures/observed/src/lib.rs"
PROBE = ROOT / "target/debug/examples/model_probe"

REFERENCE = r'''
use model_probe_fixture::equal_pairs;

fn check(data: &[u8]) {
    let expected = data.iter().map(|a| {
        data.iter().filter(|b| a == *b).count() as u64
    }).sum::<u64>();
    assert_eq!(equal_pairs(data), expected, "input: {data:?}");
}

#[test]
fn compare_with_reference() {
    for n in 0..=7 {
        for mut value in 0..3usize.pow(n) {
            let mut data = vec![0; n as usize];
            for byte in &mut data {
                *byte = (value % 3) as u8;
                value /= 3;
            }
            check(&data);
        }
    }
    let mut state = 42u64;
    for n in 0..512 {
        let mut data = vec![0; n % 129];
        for byte in &mut data {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            *byte = (state >> 32) as u8;
        }
        check(&data);
    }
    check(&(0..=255u8).collect::<Vec<_>>());
    for byte in [0, 1, 127, 255] {
        check(&vec![byte; 1024]);
    }
}
'''


def run(args, **kwargs):
    return subprocess.run(args, text=True, capture_output=True, timeout=180, **kwargs)


def snapshot():
    return {
        "gpu": gpu_snapshot(),
        "swap": run(["sysctl", "vm.swapusage"]).stdout.strip(),
        "pressure": run(["memory_pressure", "-Q"]).stdout.strip(),
        "vm_stat": run(["vm_stat"]).stdout,
    }


def gpu_snapshot():
    output = run(["ioreg", "-r", "-c", "IOAccelerator", "-l"]).stdout
    for line in output.splitlines():
        if '"PerformanceStatistics"' in line:
            return {key: int(value) for key, value in re.findall(r'"([^"]+)"=(\d+)', line)}
    return {}


def request(url, payload=None):
    req = urllib.request.Request(url)
    if payload is not None:
        req = urllib.request.Request(
            url, json.dumps(payload).encode(), {"Content-Type": "application/json"}
        )
    with urllib.request.urlopen(req, timeout=600) as response:
        return json.load(response)


def validate(diff, output, source_path=FIXTURE, reference=REFERENCE):
    patch = output / "patch.diff"
    patch.write_text(diff)
    result = run([str(PROBE), "check", str(patch), str(source_path), "encode_batch"])
    (output / "diffcheck.txt").write_text(result.stdout + result.stderr)
    if result.returncode:
        return {"diffcheck": False, "host_tests": None}
    with tempfile.TemporaryDirectory(prefix="noctum-model-probe-") as directory:
        work = Path(directory)
        (work / "src").mkdir()
        (work / "src/lib.rs").write_text(source_path.read_text())
        (work / "Cargo.toml").write_text(
            '[package]\nname = "model_probe_fixture"\nversion = "0.1.0"\nedition = "2021"\n'
        )
        run(["git", "init", "-q"], cwd=work).check_returncode()
        result = run(["git", "apply", "--check", "--recount", str(patch)], cwd=work)
        (output / "apply.txt").write_text(result.stdout + result.stderr)
        if result.returncode:
            return {"diffcheck": True, "applies": False, "host_tests": None}
        run(["git", "apply", "--recount", str(patch)], cwd=work).check_returncode()
        result = run(["cargo", "test", "--offline"], cwd=work)
        (output / "host-tests.txt").write_text(result.stdout + result.stderr)
        if result.returncode:
            return {"diffcheck": True, "applies": True, "host_tests": False}
        (work / "tests").mkdir()
        (work / "tests/reference.rs").write_text(reference)
        result = run(["cargo", "test", "--offline", "--test", "reference"], cwd=work)
        (output / "reference-tests.txt").write_text(result.stdout + result.stderr)
        (output / "patched.rs").write_text((work / "src/lib.rs").read_text())
        return {
            "diffcheck": True, "applies": True, "host_tests": True,
            "reference_tests": result.returncode == 0,
        }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--context", type=int, default=4096)
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--temperature", type=float, default=0.2)
    parser.add_argument("--reasoning-effort")
    args = parser.parse_args()
    digest = hashlib.sha256()
    with args.model.open("rb") as weights:
        for block in iter(lambda: weights.read(8 * 1024 * 1024), b""):
            digest.update(block)
    if digest.hexdigest() != args.sha256:
        raise ValueError("model checksum does not match")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    payload = json.loads(run([str(PROBE), "request"]).stdout)
    payload["temperature"] = args.temperature
    payload["cache_prompt"] = False
    (output / "request.json").write_text(json.dumps(payload, indent=2) + "\n")
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    command = [
        str(args.server.resolve()), "-m", str(args.model.resolve()),
        "--host", "127.0.0.1", "--port", str(port), "-c", str(args.context),
        "-ngl", "99", "-np", "1", "-b", "256", "-ub", "128", "--jinja",
    ]
    if args.reasoning_effort:
        command.extend(["--reasoning-effort", args.reasoning_effort])
    version = run([str(args.server), "--version"])
    report = {
        "date_utc": datetime.now(timezone.utc).isoformat(),
        "commit": run(["git", "rev-parse", "HEAD"], cwd=ROOT).stdout.strip(),
        "model_sha256": digest.hexdigest(),
        "command": command,
        "server_version": (version.stdout + version.stderr).strip(),
        "model_bytes": args.model.stat().st_size,
        "samples_requested": args.samples,
        "hardware": run(["sysctl", "-n", "machdep.cpu.brand_string", "hw.memsize"]).stdout,
        "scope": "Host inference and patch checks. No guest instruction measurement.",
        "profile_counts": "Zero values. No guest profile was collected.",
        "before": snapshot(), "samples": [],
    }
    stop = threading.Event()
    rss = []
    started = time.monotonic()
    with (output / "server.log").open("w") as log:
        process = subprocess.Popen(command, stdout=log, stderr=log, start_new_session=True)

        def monitor():
            while not stop.is_set():
                sample = run(["ps", "-o", "rss=", "-p", str(process.pid)]).stdout.strip()
                if sample.isdigit():
                    rss.append({"seconds": round(time.monotonic() - started, 3),
                                "rss_bytes": int(sample) * 1024,
                                "gpu": gpu_snapshot()})
                stop.wait(1)

        thread = threading.Thread(target=monitor, daemon=True)
        thread.start()
        try:
            base = f"http://127.0.0.1:{port}"
            while True:
                if process.poll() is not None:
                    raise RuntimeError(f"server exit: {process.returncode}")
                try:
                    if request(base + "/health").get("status") == "ok":
                        break
                except (urllib.error.URLError, TimeoutError):
                    pass
                if time.monotonic() - started > 300:
                    raise TimeoutError("server start exceeded 300 seconds")
                time.sleep(1)
            report["load_seconds"] = time.monotonic() - started
            print(f"Server ready in {report['load_seconds']:.1f} seconds", flush=True)
            for index in range(args.samples):
                sample_dir = output / str(index + 1)
                sample_dir.mkdir()
                payload["seed"] = 42 + index
                begin = time.monotonic()
                sample = {"seed": payload["seed"]}
                try:
                    response = request(base + "/v1/chat/completions", payload)
                    (sample_dir / "response.json").write_text(json.dumps(response, indent=2) + "\n")
                    sample["seconds"] = time.monotonic() - begin
                    sample["usage"] = response.get("usage")
                    sample["timings"] = response.get("timings")
                    choice = response["choices"][0]
                    sample["finish_reason"] = choice.get("finish_reason")
                    patch = json.loads(choice["message"]["content"])
                    if set(patch) != {"summary", "diff"} or not all(
                        isinstance(value, str) and value.strip() for value in patch.values()
                    ):
                        raise ValueError("response does not match the patch schema")
                    sample["schema"] = True
                    (sample_dir / "patch.diff").write_text(patch["diff"])
                except Exception as error:
                    sample["seconds"] = time.monotonic() - begin
                    sample["error"] = str(error)
                    sample["schema"] = False
                    if isinstance(error, urllib.error.HTTPError):
                        (sample_dir / "http-error.txt").write_bytes(error.read())
                report["samples"].append(sample)
                (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
                memory = run(["footprint", "-p", str(process.pid), "-f", "bytes"])
                (sample_dir / "footprint.txt").write_text(memory.stdout + memory.stderr)
                print(json.dumps(sample), flush=True)
            report["loaded"] = snapshot()
        except (Exception, KeyboardInterrupt) as error:
            report["error"] = str(error) or type(error).__name__
            print(str(error), flush=True)
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
            stop.set()
            thread.join(timeout=5)
    report["peak_sampled_rss_bytes"] = max((s["rss_bytes"] for s in rss), default=0)
    report["peak_system_gpu_bytes"] = max(
        (s["gpu"].get("In use system memory", 0) for s in rss), default=0
    )
    (output / "memory.json").write_text(json.dumps(rss, indent=2) + "\n")
    report["after"] = snapshot()
    (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    for index, sample in enumerate(report["samples"], 1):
        if sample.get("schema"):
            sample_dir = output / str(index)
            try:
                sample.update(validate((sample_dir / "patch.diff").read_text(), sample_dir))
            except Exception as error:
                sample["validation_error"] = str(error)
    (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2), flush=True)


if __name__ == "__main__":
    main()
