#!/usr/bin/env python3
"""Measure pinned upstream tests and check that the source is kept."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys


def git(repo, *args):
    return subprocess.check_output(["git", *args], cwd=repo)


def model_state(data):
    state = []
    for name in ["models", "llama.cpp"]:
        root = data / name
        if not root.exists():
            state.append((name, None))
            continue
        for path in [root, *sorted(root.resolve().rglob("*"))]:
            metadata = path.stat()
            state.append((str(path), metadata.st_size, metadata.st_mtime_ns))
    return state


def source_state(repo):
    digest = hashlib.sha256()
    names = git(repo, "ls-files", "-z", "--cached", "--others", "--exclude-standard")
    for name in sorted(set(names.split(b"\0")) - {b""}):
        path = repo / os.fsdecode(name)
        digest.update(name + b"\0")
        if path.is_symlink():
            digest.update(b"symlink\0" + os.fsencode(os.readlink(path)))
        elif path.is_file():
            digest.update(str(path.stat().st_mode & 0o777).encode() + b"\0")
            digest.update(path.read_bytes())
        else:
            digest.update(b"missing\0")
    return {
        "head": git(repo, "rev-parse", "HEAD").decode().strip(),
        "index": hashlib.sha256((repo / ".git/index").read_bytes()).hexdigest(),
        "branches": git(repo, "for-each-ref", "refs/heads").decode(),
        "source_sha256": digest.hexdigest(),
    }


def command(args, directory, label, timeout):
    print(f"{directory.name}: {label}", flush=True)
    with (directory / f"{label}.stdout").open("wb") as out:
        with (directory / f"{label}.stderr").open("wb") as err:
            process = subprocess.run(args, stdout=out, stderr=err, timeout=timeout)
    return process.returncode


def measurements(stderr):
    paths = [line.removeprefix("Results: ") for line in stderr.splitlines() if line.startswith("Results: ")]
    if not paths:
        raise RuntimeError("measurement report path is missing")
    database = Path(paths[-1]).with_name("results.db")
    with sqlite3.connect(database) as db:
        db.row_factory = sqlite3.Row
        run = dict(db.execute("SELECT * FROM runs ORDER BY id DESC LIMIT 1").fetchone())
        tests = [dict(row) for row in db.execute("SELECT * FROM test_baselines WHERE run_id = ? ORDER BY id", (run["id"],))]
        trials = db.execute("SELECT COUNT(*) FROM trials").fetchone()[0]
    stable = {
        (run["target"], test["harness"], test["test_name"]): test["insn_count"]
        for test in tests
        if test["status"] in ("measured", "skipped_small")
    }
    if not stable or trials:
        raise RuntimeError("measurement has no stable counts or contains model trials")
    if not run.get("measurement_profile", "").startswith("ram-insn-v2;"):
        raise RuntimeError("measurement needs the ram-insn-v2 guest bundle")
    return {"run": run, "tests": tests, "database": str(database), "optimization_eligible": any(t["status"] == "measured" for t in tests)}, stable


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--noctum", type=Path, default=Path("target/debug/noctum"))
    parser.add_argument("--project", action="append")
    parser.add_argument("--data-dir", type=Path)
    parser.add_argument("--repeat", type=int, default=2)
    parser.add_argument("--timeout", type=int, default=900)
    args = parser.parse_args()
    if args.repeat < 2:
        parser.error("--repeat must be at least two")
    executable = args.noctum.resolve(strict=True)
    args.output.mkdir(parents=True, exist_ok=False)
    output = args.output.resolve()
    projects = json.loads(Path(__file__).with_name("projects.json").read_text())
    if args.project:
        unknown = set(args.project) - {p["name"] for p in projects}
        if unknown:
            parser.error(f"unknown projects: {', '.join(sorted(unknown))}")
        projects = [p for p in projects if p["name"] in args.project]
    data = args.data_dir.resolve() if args.data_dir else output / "data"
    assets = model_state(data)
    config = output / "config.toml"
    config.write_text(f"data_dir = {json.dumps(str(data))}\n[gate]\nmax_tests = 2\n[guest]\nmemory_mb = 512\n")
    results = []
    for project in projects:
        directory = output / project["name"]
        directory.mkdir()
        repo = directory / "source"
        result = {"project": project, "measurements": [], "passed": False, "supported": not bool(project.get("expect_error"))}
        try:
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            subprocess.run(["git", "-C", str(repo), "fetch", "--quiet", "--depth", "1", project["url"], project["revision"]], check=True)
            subprocess.run(["git", "-C", str(repo), "checkout", "--quiet", "--detach", "FETCH_HEAD"], check=True)
            prefix = [str(executable), "--config", str(config)]
            code = command(prefix + ["init", "--repo", str(repo), "--package", project["package"]], directory, "init", args.timeout)
            if code:
                raise RuntimeError("repository setup failed; see init.stderr")
            with (repo / "noctum.toml").open("a") as settings:
                if project.get("no_default_features"):
                    settings.write("no_default_features = true\n")
                if project.get("features"):
                    settings.write(f"features = {json.dumps(project['features'])}\n")
            before = source_state(repo)
            counts = None
            executable_hash = hashlib.sha256(executable.read_bytes()).hexdigest()
            for index in range(args.repeat):
                label = f"measure-{index + 1}"
                code = command(prefix + ["measure", "--repo", str(repo), "--test", project["test"]], directory, label, args.timeout)
                expected = project.get("expect_error")
                if code and not expected:
                    raise RuntimeError(f"measurement failed; see {label}.stderr and {label}.stdout")
                if hashlib.sha256(executable.read_bytes()).hexdigest() != executable_hash:
                    raise RuntimeError("Noctum executable changed during the checks")
                if expected:
                    text = (directory / f"{label}.stdout").read_text() + (directory / f"{label}.stderr").read_text()
                    if not code or expected not in text:
                        raise RuntimeError("expected build failure was not reported")
                    result.setdefault("expected_failures", []).append({"message": expected, "noctum_sha256": executable_hash})
                    if source_state(repo) != before:
                        raise RuntimeError("failed measurement changed the source or Git state")
                    continue
                measurement, measured = measurements((directory / f"{label}.stderr").read_text())
                measurement["noctum_sha256"] = executable_hash
                if project.get("require_optimization_eligible") and not measurement["optimization_eligible"]:
                    raise RuntimeError("workload is below the optimization floor")
                result["measurements"].append(measurement)
                if counts is not None and measured != counts:
                    raise RuntimeError("instruction counts changed between commands")
                counts = measured
                if source_state(repo) != before:
                    raise RuntimeError("measurement changed source, HEAD, index, or branch references")
            if model_state(data) != assets:
                raise RuntimeError("measurement changed model files")
            result["source_state"] = before
            result["passed"] = True
        except (OSError, RuntimeError, subprocess.SubprocessError) as error:
            result["error"] = str(error)
        results.append(result)
        (directory / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        (output / "report.json").write_text(json.dumps(results, indent=2) + "\n")
        status = "PASS" if result["passed"] else "FAIL"
        if result["passed"] and not result["supported"]:
            status = "PASS (expected build failure)"
        print(f"{project['name']}: {status}", flush=True)
    return 0 if all(r["passed"] for r in results) else 1


if __name__ == "__main__":
    sys.exit(main())
