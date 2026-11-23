#!/usr/bin/env python3
"""Check worker and model process separation during a real Noctum run."""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import time


def processes():
    rows = subprocess.check_output(
        ["/bin/ps", "-Ao", "pid=,rss=,comm="], text=True
    ).splitlines()
    result = {"workers": [], "models": []}
    for line in rows:
        fields = line.strip().split(None, 2)
        if len(fields) != 3:
            continue
        pid, rss, name = fields
        if name.endswith("/MacOS/com.apple.Virtualization.VirtualMachine"):
            result["workers"].append({"pid": int(pid), "rss_kib": int(rss)})
        if name.endswith("/llama-server"):
            result["models"].append({"pid": int(pid), "rss_kib": int(rss)})
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("Set a Noctum run command.")
    initial = processes()
    if initial["workers"] or initial["models"]:
        raise RuntimeError("This check requires no active worker VM or model server.")
    spec = importlib.util.spec_from_file_location(
        "compat", Path(__file__).parent / "compatibility/run.py"
    )
    compat = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(compat)
    before = compat.source_state(args.repo.resolve())
    args.output.mkdir(parents=True, exist_ok=False)
    (args.output / "source-before.json").write_text(json.dumps(before, indent=2) + "\n")
    report = {"command": command, "states": [], "overlap": [], "passed": False}
    destination = args.output / "report.json"

    def save():
        temporary = destination.with_suffix(".partial")
        temporary.write_text(json.dumps(report, indent=2) + "\n")
        temporary.replace(destination)

    save()
    last = None
    started = time.monotonic()
    environment = {**os.environ, "PATH": "/usr/bin:/bin:/usr/sbin:/sbin"}
    with (args.output / "stdout").open("w") as stdout, (args.output / "stderr").open("w") as stderr:
        child = subprocess.Popen(command, env=environment, stdout=stdout, stderr=stderr)
        while child.poll() is None:
            sample = processes()
            state = tuple(tuple(x["pid"] for x in sample[key]) for key in ["workers", "models"])
            if state != last:
                report["states"].append({"seconds": time.monotonic() - started, **sample})
                last = state
                save()
            if sample["workers"] and sample["models"]:
                report["overlap"].append(sample)
                save()
            time.sleep(0.2)
        report["exit_code"] = child.wait()
    deadline = time.monotonic() + 10
    while any(processes().values()) and time.monotonic() < deadline:
        time.sleep(0.1)
    report["final_processes"] = processes()
    after = compat.source_state(args.repo.resolve())
    (args.output / "source-after.json").write_text(json.dumps(after, indent=2) + "\n")
    def original_state(state):
        state = dict(state)
        state["branches"] = [
            line for line in state["branches"].splitlines()
            if not line.endswith("\trefs/heads/noctum/perf")
        ]
        return state
    report["source_preserved"] = original_state(before) == original_state(after)
    report["worker_seen"] = any(row["workers"] for row in report["states"])
    report["model_seen"] = any(row["models"] for row in report["states"])
    report["passed"] = (
        report["exit_code"] == 0 and report["source_preserved"]
        and report["worker_seen"] and report["model_seen"]
        and not report["overlap"] and not any(report["final_processes"].values())
    )
    save()
    print(json.dumps({key: report[key] for key in ["passed", "exit_code", "source_preserved", "worker_seen", "model_seen"]}))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
