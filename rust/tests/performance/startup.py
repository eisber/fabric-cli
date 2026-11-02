# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Bounded offline CLI process measurements; no builds, installs, or service calls."""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any

FLAGS = ("--help", "--version")
MAX_JOB_SECONDS = 600
MAX_OUTPUT_BYTES = 262144
ENVIRONMENT_FIELDS = (
    "os_release",
    "architecture",
    "cpu_model",
    "cpu_governor",
    "background_load",
    "filesystem",
    "wsl",
    "hard_caps",
    "mutex",
)


class BenchmarkError(ValueError):
    """A request or observation is unsuitable for a successful comparison."""


@dataclass(frozen=True)
class OutputContract:
    """Independently checked bytes, not expectations learned by the benchmark."""

    sha256: str
    byte_count: int


@dataclass(frozen=True)
class Target:
    """A user-facing executable and its attested build/package provenance."""

    name: str
    executable: str
    executable_sha256: str
    source_sha: str
    tool_version: str
    target: str
    features: list[str]
    package_mode: str
    native_backend: str
    provenance_receipt: str
    contracts: dict[str, OutputContract]


@dataclass(frozen=True)
class Request:
    """Finite startup-only comparison configuration."""

    status: str
    source_sha: str
    correctness_receipt: str
    environment: dict[str, str]
    samples: int
    warmups: int
    process_timeout_seconds: float
    job_timeout_seconds: float
    targets: list[Target]


@dataclass(frozen=True)
class Observation:
    """One new process, including failures and unaggregated warmups."""

    target: str
    flag: str
    phase: str
    index: int
    wall_ms: float
    exit_code: int | None
    stdout_bytes: int
    stderr_bytes: int
    stdout_sha256: str
    valid: bool
    failure: str | None
    peak_rss_kib: int | None = None


def _object(value: Any, keys: set[str]) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        raise BenchmarkError("Request object has missing or unsupported fields")
    return value


def _text(value: Any) -> str:
    if not isinstance(value, str) or not value.strip():
        raise BenchmarkError("Metadata must contain nonempty strings")
    return value


def _digest(value: Any, length: int) -> str:
    if not isinstance(value, str) or not re.fullmatch(rf"[0-9a-f]{{{length}}}", value):
        raise BenchmarkError("Invalid source or artifact digest")
    return value


def _number(value: Any, minimum: float, maximum: float) -> float:
    if (
        isinstance(value, bool)
        or not isinstance(value, (int, float))
        or not minimum <= value <= maximum
        or not math.isfinite(value)
    ):
        raise BenchmarkError("Sample count or time bound is outside allowed limits")
    return float(value)


def _integer(value: Any, minimum: int, maximum: int) -> int:
    _number(value, minimum, maximum)
    if not isinstance(value, int):
        raise BenchmarkError("Sample and byte counts must be integers")
    return value


def load_request(value: Any) -> Request:
    """Validate the versioned schema without executing or resolving a target."""
    data = _object(
        value,
        {
            "schema_version",
            "status",
            "source_sha",
            "correctness_receipt",
            "environment",
            "samples",
            "warmups",
            "process_timeout_seconds",
            "job_timeout_seconds",
            "targets",
            "endpoint_host",
        },
    )
    if type(data["schema_version"]) is not int or data["schema_version"] != 1:
        raise BenchmarkError("Unsupported request schema version")
    if data["status"] not in ("pending", "ready") or data["endpoint_host"] is not None:
        raise BenchmarkError(
            "Only offline pending/ready startup requests are supported"
        )
    environment = _object(data["environment"], set(ENVIRONMENT_FIELDS))
    if not isinstance(data["targets"], list) or len(data["targets"]) != 2:
        raise BenchmarkError("Exactly one Rust and one Python target are required")
    targets = []
    for raw in data["targets"]:
        entry = _object(
            raw,
            {
                "name",
                "executable",
                "executable_sha256",
                "source_sha",
                "tool_version",
                "target",
                "features",
                "package_mode",
                "native_backend",
                "provenance_receipt",
                "contracts",
            },
        )
        name = _text(entry["name"])
        if name not in ("rust", "python"):
            raise BenchmarkError("Unsupported target name")
        features = entry["features"]
        if not isinstance(features, list) or any(
            not isinstance(feature, str) or not feature.strip() for feature in features
        ):
            raise BenchmarkError("Features must be an explicit list of names")
        contracts = {}
        for flag, raw_contract in _object(entry["contracts"], set(FLAGS)).items():
            contract = _object(raw_contract, {"sha256", "byte_count"})
            contracts[flag] = OutputContract(
                _digest(contract["sha256"], 64),
                _integer(contract["byte_count"], 1, MAX_OUTPUT_BYTES),
            )
        targets.append(
            Target(
                name,
                _text(entry["executable"]),
                _digest(entry["executable_sha256"], 64),
                _digest(entry["source_sha"], 40),
                _text(entry["tool_version"]),
                _text(entry["target"]),
                features,
                _text(entry["package_mode"]),
                _text(entry["native_backend"]),
                _text(entry["provenance_receipt"]),
                contracts,
            )
        )
    if {target.name for target in targets} != {"rust", "python"}:
        raise BenchmarkError("Duplicate or missing comparison target")
    request = Request(
        data["status"],
        _digest(data["source_sha"], 40),
        _text(data["correctness_receipt"]),
        {key: _text(environment[key]) for key in ENVIRONMENT_FIELDS},
        _integer(data["samples"], 30, 200),
        _integer(data["warmups"], 3, 10),
        _number(data["process_timeout_seconds"], 0.1, 30),
        _number(data["job_timeout_seconds"], 1, MAX_JOB_SECONDS),
        sorted(targets, key=lambda target: target.name),
    )
    if request.source_sha != next(
        target.source_sha for target in targets if target.name == "rust"
    ):
        raise BenchmarkError("Request source must match the measured Rust source")
    if request.status == "ready" and (
        "PENDING" in json.dumps(asdict(request))
        or any(
            target.executable_sha256 == "0" * 64
            or any(
                contract.sha256 == "0" * 64 for contract in target.contracts.values()
            )
            for target in targets
        )
    ):
        raise BenchmarkError("Ready requests must replace all pending evidence")
    return request


def file_sha256(path: Path) -> str:
    """Hash the exact executable or entry-point file without reading it all at once."""
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def preflight(request: Request) -> None:
    """Refuse pending requests, unsupported memory platforms, or mismatched files."""
    if request.status != "ready":
        raise BenchmarkError(
            "Measurements pending: correctness and execution grant required"
        )
    if sys.platform != "linux" or not Path("/usr/bin/time").is_file():
        raise BenchmarkError("Version 1 requires Linux and GNU /usr/bin/time")
    for target in request.targets:
        path = Path(target.executable)
        expected = "fab-rs" if target.name == "rust" else "fab"
        if (
            not path.is_absolute()
            or path.name != expected
            or not path.is_file()
            or not os.access(path, os.X_OK)
        ):
            raise BenchmarkError(
                "An absolute executable fab-rs or installed fab is required"
            )
        if file_sha256(path) != target.executable_sha256:
            raise BenchmarkError("Executable does not match the correctness receipt")
        if target.name == "rust" and target.package_mode != "cargo-release":
            raise BenchmarkError("Rust measurements require a release-profile build")
        if target.name == "python" and target.package_mode != "venv-installed":
            raise BenchmarkError(
                "Python measurements require an installed venv entry point"
            )


def summary(values: list[float]) -> dict[str, float | int]:
    """Use nearest-rank p95; retain every observation, including outliers."""
    if not values or any(not math.isfinite(value) or value < 0 for value in values):
        raise BenchmarkError("Cannot summarize missing or invalid observations")
    ordered = sorted(values)
    return {
        "samples": len(values),
        "median": statistics.median(values),
        "p95": ordered[math.ceil(0.95 * len(ordered)) - 1],
        "min": ordered[0],
        "max": ordered[-1],
    }


def parse_peak_rss(text: str) -> int:
    """Read GNU time's per-invocation Linux high-water RSS, in KiB."""
    matches = re.findall(
        r"^\s*Maximum resident set size \(kbytes\): (\d+)\s*$",
        text,
        re.MULTILINE,
    )
    if len(matches) != 1 or int(matches[0]) <= 0:
        raise BenchmarkError("GNU time did not report a positive per-child peak RSS")
    return int(matches[0])


def isolated_environment(home: Path) -> dict[str, str]:
    """Exclude ambient CLI state, credentials, Python imports, proxies, and tokens."""
    environment = {
        "PATH": os.defpath,
        "HOME": str(home),
        "USERPROFILE": str(home),
        "XDG_CONFIG_HOME": str(home / "config"),
        "XDG_CACHE_HOME": str(home / "cache"),
        "XDG_STATE_HOME": str(home / "state"),
        "TMPDIR": str(home),
        "TMP": str(home),
        "TEMP": str(home),
        "LC_ALL": "C",
        "LANG": "C",
        "TERM": "dumb",
        "COLUMNS": "100",
        "LINES": "24",
        "NO_COLOR": "1",
        "PYTHONNOUSERSITE": "1",
        "PYTHONDONTWRITEBYTECODE": "1",
        "PYTHONUTF8": "1",
    }
    if os.name == "nt":
        for key in ("SystemRoot", "WINDIR"):
            if key in os.environ:
                environment[key] = os.environ[key]
    return environment


def observe(
    target: Target,
    flag: str,
    phase: str,
    index: int,
    timeout: float,
    directory: Path,
    environment: dict[str, str],
    memory: bool = False,
) -> Observation:
    """Run exactly one permitted invocation; kill only its process group on timeout."""
    if flag not in FLAGS:
        raise BenchmarkError("Only help/version invocations are permitted")
    command = [target.executable, flag]
    report_path = directory / "time.txt"
    if memory:
        report_path.unlink(missing_ok=True)
        command = ["/usr/bin/time", "-v", "-o", str(report_path), "--", *command]
    failure = None
    exit_code = None
    peak_rss = None
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        process: subprocess.Popen[bytes] | None = None
        timed_out = threading.Event()

        def terminate() -> None:
            if process is not None and process.poll() is not None:
                return
            timed_out.set()
            if process is None:
                return
            if sys.platform != "win32":
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            else:
                process.kill()

        watchdog = threading.Timer(timeout, terminate)
        watchdog.daemon = True
        watchdog.start()
        start = time.perf_counter_ns()
        try:
            process = subprocess.Popen(
                command,
                stdin=subprocess.DEVNULL,
                stdout=stdout,
                stderr=stderr,
                cwd=directory,
                env=environment,
                start_new_session=os.name == "posix",
            )
            if timed_out.is_set():
                terminate()
            # wait(timeout=...) busy-polls on POSIX and distorts very short startups.
            exit_code = process.wait()
            wall_ms = (time.perf_counter_ns() - start) / 1_000_000
            if timed_out.is_set():
                failure = "timeout"
        except OSError:
            failure = "process_start_failed"
            wall_ms = (time.perf_counter_ns() - start) / 1_000_000
        finally:
            watchdog.cancel()
            if process is not None and process.poll() is None:
                terminate()
                process.wait()
            watchdog.join()
        stdout_bytes, stderr_bytes = stdout.tell(), stderr.tell()
        stdout.seek(0)
        digest = (
            hashlib.file_digest(stdout, "sha256").hexdigest()
            if stdout_bytes <= MAX_OUTPUT_BYTES
            else ""
        )
    contract = target.contracts[flag]
    if failure is None:
        if exit_code != 0:
            failure = "nonzero_exit"
        elif stderr_bytes:
            failure = "unexpected_stderr"
        elif stdout_bytes != contract.byte_count or digest != contract.sha256:
            failure = "invalid_stdout"
        elif memory:
            try:
                peak_rss = parse_peak_rss(report_path.read_text(encoding="utf-8"))
            except (OSError, UnicodeError, BenchmarkError):
                failure = "invalid_peak_rss"
    return Observation(
        target.name,
        flag,
        phase,
        index,
        wall_ms,
        exit_code,
        stdout_bytes,
        stderr_bytes,
        digest,
        failure is None,
        failure,
        peak_rss,
    )


def run(request: Request, receipt: dict[str, Any]) -> None:
    """Measure paired cases serially under one finite job-wide deadline."""
    deadline = time.monotonic() + request.job_timeout_seconds
    preflight(request)
    observations: list[Observation] = []
    receipt["observations"] = []
    with tempfile.TemporaryDirectory(prefix="fab-startup-") as temporary:
        root = Path(temporary)
        homes = {target.name: root / target.name for target in request.targets}
        for home in homes.values():
            home.mkdir()
        for flag in FLAGS:
            for phase, count in (
                ("first_observed_case", 1),
                ("latency_warmup", request.warmups),
                ("latency", request.samples),
                ("memory_warmup", request.warmups),
                ("memory", request.samples),
            ):
                for index in range(count):
                    targets = (
                        request.targets if index % 2 == 0 else request.targets[::-1]
                    )
                    for target in targets:
                        remaining = deadline - time.monotonic()
                        if remaining <= 0:
                            raise BenchmarkError(
                                "Job deadline exhausted; comparison incomplete"
                            )
                        home = homes[target.name]
                        observation = observe(
                            target,
                            flag,
                            phase,
                            index,
                            min(request.process_timeout_seconds, remaining),
                            home,
                            isolated_environment(home),
                            memory=phase.startswith("memory"),
                        )
                        observations.append(observation)
                        receipt["observations"].append(asdict(observation))
                        if not observation.valid:
                            raise BenchmarkError(
                                f"{target.name} {flag} {phase}: {observation.failure}"
                            )
    # Do not publish partial summaries for a failed or incomplete comparison.
    receipt["summaries"] = [
        {
            "target": target.name,
            "flag": flag,
            "wall_ms": summary(
                [
                    row.wall_ms
                    for row in observations
                    if row.target == target.name
                    and row.flag == flag
                    and row.phase == "latency"
                ]
            ),
            "peak_rss_kib": summary(
                [
                    float(row.peak_rss_kib)
                    for row in observations
                    if row.target == target.name
                    and row.flag == flag
                    and row.phase == "memory"
                    and row.peak_rss_kib is not None
                ]
            ),
        }
        for target in request.targets
        for flag in FLAGS
    ]
    receipt["status"] = "passed"


def main(argv: list[str] | None = None) -> int:
    """Write an exclusive JSON receipt; errors never become successful samples."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--request", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--validate-only", action="store_true")
    args = parser.parse_args(argv)
    receipt: dict[str, Any] = {
        "schema_version": 1,
        "status": "failed",
        "started_utc": datetime.now(timezone.utc).isoformat(),
        "harness_sha256": file_sha256(Path(__file__)),
        "request_sha256": None,
        "harness_runtime": sys.version,
        "platform": platform.platform(),
        "architecture": platform.machine(),
        "endpoint_host": None,
        "units": {"wall": "ms", "peak_rss": "KiB"},
        "observations": [],
    }
    # Reserve the destination before executing; never overwrite existing evidence.
    with args.output.open("x", encoding="utf-8") as output:
        try:
            raw = args.request.read_bytes()
            receipt["request_sha256"] = hashlib.sha256(raw).hexdigest()
            request = load_request(json.loads(raw))
            receipt["request"] = asdict(request)
            if args.validate_only:
                receipt["status"] = "validated_only"
            else:
                run(request, receipt)
        except (BenchmarkError, OSError, UnicodeError, json.JSONDecodeError) as error:
            receipt["error"] = (
                str(error)
                if isinstance(error, BenchmarkError)
                else type(error).__name__
            )
        finally:
            receipt["finished_utc"] = datetime.now(timezone.utc).isoformat()
            json.dump(receipt, output, indent=2, allow_nan=False)
            output.write("\n")
    return 0 if receipt["status"] in ("passed", "validated_only") else 1


if __name__ == "__main__":
    raise SystemExit(main())
