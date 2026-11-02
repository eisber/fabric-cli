# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Verify the existing preview bundle offline; optionally smoke a trusted copy.

This tool neither downloads nor extracts archives, builds code, discovers
credentials, installs software, or certifies publisher identity. Expectations
must come from separately trusted source/build/provider receipts, not the bundle.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, NoReturn

TOOLCHAIN = "1.99.1"
MAX_METADATA = 1024 * 1024
MAX_BINARY = 512 * 1024 * 1024
MAX_OUTPUT = 1024 * 1024
SMOKE_TIMEOUT = 10.0
TARGETS = {
    "x86_64-pc-windows-msvc": ("windows-2025", "Windows", "X64", "Windows"),
    "x86_64-unknown-linux-gnu": ("ubuntu-24.04", "Linux", "X64", "Linux"),
    "aarch64-apple-darwin": ("macos-15", "macOS", "ARM64", "Darwin"),
}
CASES = (
    ("--help",),
    ("-h",),
    ("--version",),
    ("-V",),
    (),
    ("unknown-command",),
    ("--not-a-flag",),
    ("--client-secret", "must-not-be-echoed"),
)
INVALID = {
    "error": {
        "code": "invalid_arguments",
        "message": "Invalid arguments. Run fab-rs --help for available commands and usage.",
    }
}
EXPECT_KEYS = {
    "source_commit",
    "workflow_ref",
    "target",
    "variant",
    "toolchain",
    "rustc_sha256",
    "cargo_sha256",
    "lockfile_sha256",
    "manifest_sha256",
    "binary_sha256",
    "binary_bytes",
    "run_id",
    "run_attempt",
    "version",
}


class ReleaseError(ValueError):
    """A stable diagnostic that contains no bundle text or process output."""


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise ReleaseError(message)


def _sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _matches(pattern: str, value: Any) -> bool:
    return isinstance(value, str) and re.fullmatch(pattern, value) is not None


def _keys(value: Any, keys: set[str], label: str) -> dict[str, Any]:
    _require(type(value) is dict and set(value) == keys, f"Invalid {label} fields")
    return value


def _unique_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        _require(key not in result, "Duplicate JSON field")
        result[key] = value
    return result


def _json(raw: bytes) -> Any:
    try:
        return json.loads(raw.decode("utf-8"), object_pairs_hook=_unique_pairs)
    except ReleaseError:
        raise
    except (UnicodeError, ValueError, RecursionError) as error:
        raise ReleaseError("Invalid UTF-8 JSON") from error


def _plain(path: Path, directory: bool = False) -> os.stat_result:
    info = path.lstat()
    _require(
        not path.is_symlink()
        and not getattr(info, "st_file_attributes", 0)
        & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400),
        "Links and reparse points are not permitted",
    )
    _require(
        stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode),
        "Expected a plain directory" if directory else "Expected a regular file",
    )
    if not directory:
        _require(info.st_nlink == 1, "Hard-linked files are not permitted")
    return info


def _absolute(path: Path) -> Path:
    _require(path.is_absolute(), "Use absolute input paths")
    _require(".." not in path.parts, "Parent traversal is not permitted")
    for parent in reversed(path.parents):
        _plain(parent, directory=True)
    return path


def _read(path: Path, limit: int) -> bytes:
    _require(_plain(path).st_size <= limit, "Metadata exceeds size limit")
    with path.open("rb") as stream:
        data = stream.read(limit + 1)
    _require(len(data) <= limit, "Metadata exceeds size limit")
    return data


def _hash_file(path: Path, limit: int) -> tuple[str, int]:
    _require(_plain(path).st_size <= limit, "File exceeds size limit")
    digest = hashlib.sha256()
    length = 0
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            length += len(chunk)
            _require(length <= limit, "File exceeds size limit")
            digest.update(chunk)
    return digest.hexdigest(), length


def load_expectations(path: Path) -> dict[str, Any]:
    """Load strict, separately trusted expectations without contacting a service."""
    expected = _keys(
        _json(_read(_absolute(path), MAX_METADATA)), EXPECT_KEYS, "expectation"
    )
    _require(_matches(r"[a-f0-9]{40}", expected["source_commit"]), "Invalid source pin")
    for key in EXPECT_KEYS:
        if key.endswith("_sha256"):
            _require(_matches(r"[a-f0-9]{64}", expected[key]), "Invalid digest pin")
    _require(
        type(expected["binary_bytes"]) is int
        and 0 < expected["binary_bytes"] <= MAX_BINARY,
        "Invalid binary length pin",
    )
    _require(
        isinstance(expected["target"], str) and expected["target"] in TARGETS,
        "Unsupported target",
    )
    _require(expected["variant"] in ("default", "native-read"), "Unsupported variant")
    _require(
        expected["variant"] == "default"
        or expected["target"] == "x86_64-pc-windows-msvc",
        "Native-read requires Windows x64",
    )
    _require(expected["toolchain"] == TOOLCHAIN, "Unsupported toolchain")
    for key in ("run_id", "run_attempt"):
        _require(
            _matches(r"[1-9][0-9]{0,19}", expected[key]), "Invalid provider run pin"
        )
    _require(
        _matches(
            r"eisber/fabric-cli/\.github/workflows/rust-preview-build\.yml@"
            r"[A-Za-z0-9_./-]{1,200}",
            expected["workflow_ref"],
        ),
        "Invalid workflow reference pin",
    )
    _require(
        _matches(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?", expected["version"]),
        "Invalid executable version pin",
    )
    return expected


def verify_bundle(bundle: Path, expected: dict[str, Any]) -> dict[str, Any]:
    """Verify an extracted schema-1 preview bundle; never execute its contents."""
    _absolute(bundle)
    _plain(bundle, directory=True)
    target, variant = expected["target"], expected["variant"]
    native = variant == "native-read"
    filename = f"fab-rs-{variant}" + (".exe" if "windows" in target else "")
    files = {filename, "manifest.json", "SHA256SUMS", "LICENSE", "README.md"}
    _require(
        {entry.name for entry in bundle.iterdir()} == files, "Bundle allowlist mismatch"
    )
    hashes: dict[str, str] = {}
    lengths: dict[str, int] = {}
    for name in sorted(files):
        hashes[name], lengths[name] = _hash_file(
            bundle / name, MAX_BINARY if name == filename else MAX_METADATA
        )
    _require(
        hashes["manifest.json"] == expected["manifest_sha256"],
        "Manifest digest does not match trusted pin",
    )
    sums = _read(bundle / "SHA256SUMS", 4096)
    canonical = "".join(
        f"{hashes[name]}  {name}\n" for name in sorted(files - {"SHA256SUMS"})
    ).encode("ascii")
    _require(sums == canonical, "Checksum list must match all four sorted file digests")
    manifest = _keys(
        _json(_read(bundle / "manifest.json", MAX_METADATA)),
        {
            "schema_version",
            "artifact_name",
            "source",
            "build",
            "binary",
            "capabilities",
            "smoke",
            "limitations",
            "ci",
        },
        "manifest",
    )
    _require(
        type(manifest["schema_version"]) is int and manifest["schema_version"] == 1,
        "Unsupported manifest schema",
    )
    source = _keys(
        manifest["source"],
        {"repository", "commit", "workflow_ref", "workflow_commit"},
        "source",
    )
    _require(
        source
        == {
            "repository": "eisber/fabric-cli",
            "commit": expected["source_commit"],
            "workflow_ref": expected["workflow_ref"],
            "workflow_commit": expected["source_commit"],
        },
        "Source or workflow identity mismatch",
    )
    build = _keys(
        manifest["build"],
        {
            "target",
            "features",
            "default_features",
            "profile",
            "toolchain",
            "rustc",
            "cargo",
            "lockfile_sha256",
            "runner",
            "os",
            "architecture",
            "image_os",
            "image_version",
        },
        "build",
    )
    runner, operating_system, architecture, _ = TARGETS[target]
    required = {
        "target": target,
        "features": ["native-read"] if native else [],
        "profile": "release",
        "toolchain": TOOLCHAIN,
        "lockfile_sha256": expected["lockfile_sha256"],
        "runner": runner,
        "os": operating_system,
        "architecture": architecture,
    }
    _require(
        all(build[key] == value for key, value in required.items()),
        "Build identity mismatch",
    )
    _require(build["default_features"] is False, "Default features must be disabled")
    for key in ("rustc", "cargo"):
        value = build[key]
        _require(isinstance(value, str), "Invalid compiler identity")
        try:
            digest = _sha(value.encode("utf-8"))
        except UnicodeError as error:
            raise ReleaseError("Invalid compiler identity encoding") from error
        _require(
            digest == expected[f"{key}_sha256"],
            "Compiler digest mismatch",
        )
        _require(value.startswith(f"{key} {TOOLCHAIN} "), "Compiler version mismatch")
    _require(
        f"host: {target}" in build["rustc"].splitlines(),
        "Compiler host mismatch",
    )
    for key in ("image_os", "image_version"):
        _require(
            build[key] is None or isinstance(build[key], str),
            "Invalid runner image metadata",
        )
    binary = _keys(manifest["binary"], {"filename", "sha256", "bytes"}, "binary")
    _require(
        binary["filename"] == filename
        and binary["sha256"] == hashes[filename] == expected["binary_sha256"]
        and type(binary["bytes"]) is int
        and binary["bytes"] == lengths[filename] == expected["binary_bytes"],
        "Binary name, hash or length mismatch",
    )
    capabilities = _keys(
        manifest["capabilities"],
        {
            "variant",
            "bounded_windows_read_host",
            "broker_commands_compiled",
            "live_service_verified",
            "full_python_cli_parity",
        },
        "capability",
    )
    _require(
        capabilities["variant"] == variant
        and capabilities["bounded_windows_read_host"] is native
        and capabilities["broker_commands_compiled"] is native
        and capabilities["live_service_verified"] is False
        and capabilities["full_python_cli_parity"] is False,
        "Unsupported capability claim",
    )
    run_id, attempt = expected["run_id"], expected["run_attempt"]
    _require(
        manifest["ci"]
        == {
            "run_url": f"https://github.com/eisber/fabric-cli/actions/runs/{run_id}",
            "run_id": run_id,
            "run_attempt": attempt,
        },
        "Provider run mismatch",
    )
    artifact = (
        f"fab-rs-{expected['source_commit']}-{target}-{variant}-rust-{TOOLCHAIN}"
        f"-run-{run_id}-{attempt}"
    )
    _require(manifest["artifact_name"] == artifact, "Artifact identity mismatch")
    limitations = manifest["limitations"]
    _require(
        isinstance(limitations, list)
        and len(limitations) >= 6
        and all(isinstance(item, str) and item.strip() for item in limitations),
        "Missing preview limitations",
    )
    smoke = _keys(manifest["smoke"], {"environment", "cases"}, "smoke")
    _require(smoke["environment"] == "isolated-allowlist", "Unknown smoke environment")
    _require(
        isinstance(smoke["cases"], list) and len(smoke["cases"]) == len(CASES),
        "Invalid smoke case count",
    )
    for args, case in zip(CASES, smoke["cases"]):
        _keys(
            case,
            {"arguments", "exit_code", "stdout_sha256", "stderr_sha256"},
            "smoke case",
        )
        _require(
            case["arguments"] == list(args)
            and type(case["exit_code"]) is int
            and case["exit_code"]
            == (0 if args and args[0] in ("--help", "-h", "--version", "-V") else 2)
            and all(
                _matches(r"[a-f0-9]{64}", case[key])
                for key in ("stdout_sha256", "stderr_sha256")
            ),
            "Invalid recorded smoke contract",
        )
    return {
        "schema_version": 1,
        "result": "bundle-verified",
        "source_commit": expected["source_commit"],
        "target": target,
        "variant": variant,
        "artifact_name": artifact,
        "binary_filename": filename,
        "binary_bytes": lengths[filename],
        "file_sha256": hashes,
        "publisher_signature_verified": False,
        "clean_machine_verified": False,
        "live_service_verified": False,
    }


def _native_host(target: str) -> bool:
    _, _, architecture, system = TARGETS[target]
    machines = {"X64": {"amd64", "x86_64"}, "ARM64": {"arm64", "aarch64"}}
    return (
        platform.system() == system
        and platform.machine().lower() in machines[architecture]
    )


def _environment(sandbox: Path) -> dict[str, str]:
    home = sandbox / "home"
    home.mkdir()
    environment = {
        "HOME": str(home),
        "USERPROFILE": str(home),
        "XDG_CONFIG_HOME": str(home),
        "XDG_CACHE_HOME": str(home),
        "TEMP": str(sandbox),
        "TMP": str(sandbox),
        "TMPDIR": str(sandbox),
        "PATH": "",
        "NO_COLOR": "1",
        "NODE_V8_COVERAGE": "",
    }
    if os.name == "nt":
        for key in ("SystemRoot", "WINDIR"):
            if key in os.environ:
                environment[key] = os.environ[key]
    return environment


def _run(
    executable: Path,
    args: tuple[str, ...],
    sandbox: Path,
    environment: dict[str, str],
    timeout: float,
) -> tuple[int, bytes, bytes]:
    # Files bound parent memory; polling also caps a noisy child's disk output.
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        try:
            process = subprocess.Popen(
                [str(executable), *args],
                cwd=sandbox,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=stdout,
                stderr=stderr,
                shell=False,
                creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
            )
        except OSError as error:
            raise ReleaseError("Copied executable could not start") from error
        deadline = time.monotonic() + timeout
        try:
            while True:
                _require(
                    os.fstat(stdout.fileno()).st_size <= MAX_OUTPUT
                    and os.fstat(stderr.fileno()).st_size <= MAX_OUTPUT,
                    "Smoke output exceeded limit",
                )
                remaining = deadline - time.monotonic()
                _require(remaining > 0, "Smoke timed out")
                try:
                    process.wait(timeout=min(remaining, 0.02))
                    break
                except subprocess.TimeoutExpired:
                    continue
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)
        stdout.seek(0)
        stderr.seek(0)
        out, err = stdout.read(MAX_OUTPUT + 1), stderr.read(MAX_OUTPUT + 1)
        _require(
            len(out) <= MAX_OUTPUT and len(err) <= MAX_OUTPUT,
            "Smoke output exceeded limit",
        )
        return process.returncode, out, err


def smoke_bundle(
    bundle: Path, expected: dict[str, Any], timeout: float = SMOKE_TIMEOUT
) -> dict[str, Any]:
    """Copy and run eight offline contracts with no inherited tools or credentials.

    This is not a security or network sandbox. Execute only an authorized,
    trusted candidate on an isolated host, never an arbitrary downloaded binary.
    """
    _require(0 < timeout <= SMOKE_TIMEOUT, "Smoke timeout must be at most ten seconds")
    receipt = verify_bundle(bundle, expected)
    _require(
        _native_host(expected["target"]),
        "Smoke requires the matching native OS and architecture",
    )
    with tempfile.TemporaryDirectory(prefix="fab-release-smoke-") as directory:
        root = Path(directory).resolve()
        executable = root / receipt["binary_filename"]
        shutil.copyfile(bundle / executable.name, executable)
        if os.name != "nt":
            executable.chmod(0o700)
        original = (expected["binary_sha256"], expected["binary_bytes"])
        _require(
            _hash_file(executable, MAX_BINARY) == original, "Copied executable changed"
        )
        sandbox = root / "work"
        sandbox.mkdir()
        environment = _environment(sandbox)
        cases = []
        for args in CASES:
            code, out, err = _run(executable, args, sandbox, environment, timeout)
            help_case = args in CASES[:2]
            version_case = args in CASES[2:4]
            _require(
                code == (0 if help_case or version_case else 2),
                "Smoke exit contract failed",
            )
            if help_case:
                _require(
                    not err
                    and all(
                        value in out
                        for value in (b"Usage: fab-rs", b"--help", b"--version")
                    ),
                    "Smoke help contract failed",
                )
            elif version_case:
                _require(
                    not err and out == f"fab-rs {expected['version']}\n".encode(),
                    "Smoke version contract failed",
                )
            else:
                _require(
                    not out
                    and err.endswith(b"\n")
                    and b"must-not-be-echoed" not in err,
                    "Smoke stream or redaction contract failed",
                )
                _require(_json(err) == INVALID, "Smoke error envelope failed")
            cases.append(
                {
                    "arguments": list(args),
                    "exit_code": code,
                    "stdout_sha256": _sha(out),
                    "stderr_sha256": _sha(err),
                }
            )
        _require(
            _hash_file(executable, MAX_BINARY) == original, "Smoke modified executable"
        )
    _require(verify_bundle(bundle, expected) == receipt, "Bundle changed during smoke")
    receipt.update(
        result="copied-executable-smoke-passed",
        smoke={
            "environment": "isolated-allowlist",
            "timeout_seconds": timeout,
            "cases": cases,
        },
    )
    return receipt


class _Parser(argparse.ArgumentParser):
    def error(self, message: str) -> NoReturn:
        raise ReleaseError("Invalid arguments; run --help for usage")


def main(argv: list[str] | None = None) -> int:
    """Write a bounded JSON receipt or sanitized diagnostic; perform no live setup."""
    parser = _Parser(description=__doc__)
    parser.add_argument(
        "--bundle", type=Path, required=True, help="Absolute extracted bundle directory"
    )
    parser.add_argument(
        "--expect",
        type=Path,
        required=True,
        help="Absolute separately trusted expectations JSON",
    )
    parser.add_argument(
        "--smoke",
        action="store_true",
        help="Execute an authorized trusted copy on the matching host",
    )
    try:
        args = parser.parse_args(argv)
        expected = load_expectations(args.expect)
        receipt = (smoke_bundle if args.smoke else verify_bundle)(args.bundle, expected)
    except ReleaseError as error:
        print(
            json.dumps(
                {
                    "error": {
                        "code": "release_verification_failed",
                        "message": str(error),
                    }
                }
            ),
            file=sys.stderr,
        )
        return 1
    except OSError as error:
        print(
            json.dumps({"error": {"code": "release_io_failed", "errno": error.errno}}),
            file=sys.stderr,
        )
        return 1
    print(json.dumps(receipt, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
