# Copyright (c) Microsoft Corporation. Licensed under the MIT License.
"""Fail closed on cross-builds and record observations, not minimum OS claims."""

import argparse
import json
import os
from pathlib import Path
import platform
import subprocess
import tomllib

TARGETS = {
    "x86_64-pc-windows-msvc": ("Windows", {"amd64", "x86_64"}),
    "x86_64-unknown-linux-gnu": ("Linux", {"amd64", "x86_64"}),
    "aarch64-apple-darwin": ("Darwin", {"arm64", "aarch64"}),
    "x86_64-apple-darwin": ("Darwin", {"amd64", "x86_64"}),
}
ROOT = Path(__file__).resolve().parents[1]


def command(*args: str) -> str:
    """Run a bounded evidence command."""
    return subprocess.check_output(args, text=True, timeout=60).strip()


def evidence(target: str) -> dict:
    """Verify actual host/compiler against the pinned native contract."""
    system, architectures = TARGETS[target]
    if platform.system() != system or platform.machine().lower() not in architectures:
        raise ValueError(f"Native host mismatch for {target}")
    if system == "Darwin":
        translated = command("sysctl", "-in", "sysctl.proc_translated")
        if translated not in ("", "0"):
            raise ValueError("Translated macOS execution is not native host evidence")
    if system == "Windows":
        native_arch = os.environ.get("PROCESSOR_ARCHITEW6432", platform.machine()).lower()
        if native_arch not in architectures:
            raise ValueError("Emulated Windows execution is not native host evidence")
    toolchain = tomllib.loads((ROOT / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    rustc = command("rustc", "-vV")
    fields = dict(line.split(": ", 1) for line in rustc.splitlines() if ": " in line)
    if fields.get("host") != target or fields.get("release") != toolchain:
        raise ValueError("rustc host/release does not match target and pinned toolchain")
    installed = command("rustup", "target", "list", "--installed").splitlines()
    if target not in installed:
        raise ValueError("Native Rust target not installed")
    observed = {
        "target": target,
        "toolchain": toolchain,
        "rustc": rustc,
        "cargo": command("cargo", "--version"),
        "os": platform.platform(),
        "architecture": platform.machine(),
        "runner_image": os.environ.get("ImageOS"),
        "runner_image_version": os.environ.get("ImageVersion"),
        "minimum_os": None,
    }
    if system == "Linux":
        observed["glibc_observed"] = command("getconf", "GNU_LIBC_VERSION")
    if system == "Darwin":
        observed["macos_observed"] = command("sw_vers", "-productVersion")
    return observed


def main() -> None:
    """Record verified native build-host evidence."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = evidence(args.target)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
