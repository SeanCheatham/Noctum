#!/usr/bin/env python3
"""Check two different Rust tasks with the production model code."""

import argparse
import json
from pathlib import Path
import subprocess
import time

import probe

HEX_REFERENCE = r'''
use model_probe_fixture::hex_encode;
#[test]
fn compare_with_reference() {
    let mut state = 42u64;
    for len in 0..512 {
        let mut data = vec![0u8; len];
        for byte in &mut data {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            *byte = (state >> 32) as u8;
        }
        let expected = data.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join("");
        assert_eq!(hex_encode(&data), expected);
    }
}
'''

ROTATION_REFERENCE = r'''
use model_probe_fixture::is_rotation;
fn reference(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    if a.is_empty() { return true; }
    (0..a.len()).any(|shift| {
        a[shift..].iter().chain(a[..shift].iter()).copied().collect::<Vec<_>>() == b
    })
}
fn input(mut value: usize, len: usize) -> Vec<u8> {
    (0..len).map(|_| { let b = (value % 2) as u8; value /= 2; b }).collect()
}
#[test]
fn compare_with_reference() {
    for len in 0..=7 {
        for a in 0..(1 << len) {
            for b in 0..(1 << len) {
                let a = input(a, len);
                let b = input(b, len);
                assert_eq!(is_rotation(&a, &b), reference(&a, &b));
            }
        }
    }
    for len in 0..128 {
        let a: Vec<_> = (0..len).map(|n| n as u8).collect();
        let b: Vec<_> = (0..len + 1).map(|n| n as u8).collect();
        assert_eq!(is_rotation(&a, &b), reference(&a, &b));
    }
}
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    executable = probe.ROOT / "target/release/examples/model_probe"
    probe.PROBE = executable
    report = []
    for name, function, reference in [
        ("hex", "hex_encode", HEX_REFERENCE),
        ("rotation", "is_rotation", ROTATION_REFERENCE),
    ]:
        source = Path(__file__).resolve().parent / "fixtures" / f"{name}.rs"
        output = (args.output / name).resolve()
        output.mkdir()
        command = [str(executable), "complete", str(args.data), str(source), function, "encode_batch"]
        begin = time.monotonic()
        result = subprocess.run(command, capture_output=True, text=True, timeout=1800)
        (output / "response.json").write_text(result.stdout)
        (output / "stderr.txt").write_text(result.stderr)
        row = {"task": name, "command": command, "seconds": time.monotonic() - begin}
        if result.returncode:
            row["error"] = result.stderr
        else:
            response = json.loads(result.stdout)
            row.update(probe.validate(response["diff"], output, source, reference))
        report.append(row)
        (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(row), flush=True)
    if not all(row.get("reference_tests") is True for row in report):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
