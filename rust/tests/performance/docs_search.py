# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Bounded offline embedded-docs measurements; never builds or discovers credentials.

Supply absolute --baseline, --engine-only, --embedded release executable paths,
--output (new JSON), --source-sha, --execution-grant (owner's admission receipt),
and --build-metadata JSON. Optional --reader-benchmark runs the same-reader Rust
example, and --ram-control additionally runs THAT executable with --copy, never
the CLI. --queries defaults to the corpus-pinned fixture; its schema is in-file.

Build metadata schema 1:
{"schema_version":1,"toolchain":"rustc ...","target_triple":"...",
 "profile":"release","receipt":"immutable build/correctness evidence reference",
 "variants":{"baseline":{"source_sha":"40 lowercase hex","sha256":"64 hex",
 "features":[]},"engine_only":{...},"embedded":{...},"reader":{...}}}
The reader entry is required iff --reader-benchmark is supplied. Shared fields
attest matched builds; this harness verifies executable digests, not the truth
of a toolchain attestation. Every variant source SHA must match --source-sha.
Baseline features must exclude docs-search; engine_only and embedded must include
it. The three CLI artifacts must have distinct hashes. Independent corpus-info
probes verify absent-command, engine-without-corpus, and embedded-corpus states
before accepting size deltas or starting timed comparisons.
An optional --manifest-sha256 pins sorted, compact, UTF-8 JSON of the manifest.

Startup --version uses independent exact bytes. Help is structurally checked,
NOT a learned snapshot or a byte-parity assertion. New processes are process-cold
only; OS caches are uncontrolled. Same-reader times are separately reported.
These source-grounded probes are not telemetry or observed user traffic.
"""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import random
import re
import shutil
import signal
import statistics
import subprocess
import sys
import threading
import time
from typing import Any
import uuid

SOURCE_COMMIT = "3ae7afc73169ac57b874d5aee09217bb35a4f8fc"
VERSION = b"fab-rs 0.1.0\n"
# Independent CLI/library error contracts, never learned from a measured target.
VARIANT_ERRORS = {
    "baseline": {
        "error": {
            "code": "invalid_arguments",
            "message": "Invalid arguments. Run fab-rs --help for available commands and usage.",
        }
    },
    "engine_only": {
        "error": {
            "code": "docs_search_not_configured",
            "message": (
                "No documentation index is embedded. Rebuild with FAB_DOCS_INDEX "
                "set to a prepared bundle directory."
            ),
        }
    },
}
SEED = 20261112
MAX_FIXTURE_BYTES = 131072
MAX_OUTPUT_BYTES = 32 * 1024 * 1024
STAGES = ("parse_us", "search_us", "fetch_us", "total_us")
DEFAULT_QUERIES = (
    Path(__file__).resolve().parents[2]
    / "engine"
    / "control-plane"
    / "search"
    / "docs"
    / "tests"
    / "fixtures"
    / "queries.json"
)


class BenchmarkError(ValueError):
    """Invalid input or failed observation; never a successful performance sample."""


def _object(value: Any, keys: set[str] | None = None) -> dict[str, Any]:
    if not isinstance(value, dict) or (keys is not None and set(value) != keys):
        raise BenchmarkError("Invalid object shape or unsupported fields")
    return value


def _text(value: Any) -> str:
    if not isinstance(value, str) or not value.strip():
        raise BenchmarkError("Expected nonempty metadata text")
    return value


def _integer(value: Any, minimum: int, maximum: int) -> int:
    if type(value) is not int or not minimum <= value <= maximum:
        raise BenchmarkError("Integer outside allowed bounds")
    return value


def _duration(value: Any) -> int:
    return _integer(value, 0, 2**64 - 1)


def _digest(value: Any, length: int) -> str:
    if not isinstance(value, str) or not re.fullmatch(rf"[0-9a-f]{{{length}}}", value):
        raise BenchmarkError("Invalid source or artifact digest")
    return value


def _finite(value: Any) -> float:
    if (
        isinstance(value, bool)
        or not isinstance(value, (int, float))
    ):
        raise BenchmarkError("Expected a finite number")
    try:
        converted = float(value)
    except OverflowError as error:
        raise BenchmarkError("Expected a finite number") from error
    if not math.isfinite(converted):
        raise BenchmarkError("Expected a finite number")
    return converted


def _pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise BenchmarkError("Duplicate JSON field")
        result[key] = value
    return result


def decode_json(raw: bytes) -> Any:
    """Reject duplicate fields, nonfinite JSON and invalid UTF-8."""
    def invalid_constant(_: str) -> Any:
        raise BenchmarkError("Nonfinite JSON")

    def finite_float(value: str) -> float:
        return _finite(float(value))

    return json.loads(
        raw.decode("utf-8"),
        object_pairs_hook=_pairs,
        parse_constant=invalid_constant,
        parse_float=finite_float,
    )


def read_json(path: Path, maximum: int) -> tuple[Any, str]:
    """Read finite-size evidence without permitting a growing file to evade bounds."""
    with path.open("rb") as stream:
        raw = stream.read(maximum + 1)
    if len(raw) > maximum:
        raise BenchmarkError("JSON file exceeds byte budget")
    return decode_json(raw), hashlib.sha256(raw).hexdigest()


def canonical_digest(value: Any) -> str:
    """Hash sorted compact UTF-8 JSON, independently of pretty-printing."""
    return hashlib.sha256(
        json.dumps(
            value, sort_keys=True, separators=(",", ":"), ensure_ascii=False,
            allow_nan=False,
        ).encode("utf-8")
    ).hexdigest()


def artifact(path: Path) -> dict[str, Any]:
    """Hash the exact executable bytes, not its package or source size."""
    digest = hashlib.sha256()
    count = 0
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            count += len(chunk)
            digest.update(chunk)
    return {"path": str(path), "bytes": count, "sha256": digest.hexdigest()}


def summary(values: list[float | int]) -> dict[str, float | int]:
    """Report median p50 and nearest-rank p95, retaining every outlier."""
    if not values or any(_finite(value) < 0 for value in values):
        raise BenchmarkError("Cannot summarize empty or negative measurements")
    ordered = sorted(values)
    return {
        "samples": len(values),
        "p50": statistics.median(values),
        "p95": ordered[math.ceil(0.95 * len(values)) - 1],
        "min": ordered[0],
        "max": ordered[-1],
    }


def _source_path(value: Any) -> str:
    value = _text(value)
    if (
        not value.startswith("docs/")
        or not value.endswith((".md", ".yml", ".yaml"))
        or "\\" in value
        or any(part in ("", ".", "..") for part in value.split("/"))
    ):
        raise BenchmarkError("Invalid source article path")
    return value


def load_queries(value: Any) -> list[dict[str, Any]]:
    """Validate independent, source-pinned expectations before any child executes."""
    fixture = _object(
        value,
        {"copyright", "schema_version", "description", "schema", "source_commit", "cases"},
    )
    if (
        type(fixture["schema_version"]) is not int
        or fixture["schema_version"] != 1
        or fixture["source_commit"] != SOURCE_COMMIT
    ):
        raise BenchmarkError("Unsupported fixture schema or source pin")
    for key in ("copyright", "description", "schema"):
        _text(fixture[key])
    cases = fixture["cases"]
    if not isinstance(cases, list) or not 1 <= len(cases) <= 32:
        raise BenchmarkError("Expected 1..32 query cases")
    ids = set()
    for case in cases:
        _object(case, {"id", "text", "limit", "workload", "fuzzy", "expected", "grounding"})
        if (
            not isinstance(case["id"], str)
            or not re.fullmatch(r"[a-z0-9-]{1,80}", case["id"])
            or case["id"] in ids
        ):
            raise BenchmarkError("Invalid or duplicate case id")
        ids.add(case["id"])
        text = _text(case["text"])
        if (
            len(text.encode("utf-8")) > 2048
            or len(re.findall(r"\w+", text)) > 64
            or any(token in text for token in ("*", "?", ":/"))
        ):
            raise BenchmarkError("Query exceeds the supported syntax/budget")
        depth = 0
        for character in text:
            if character == "(":
                depth += 1
            elif character == ")":
                depth -= 1
            if not 0 <= depth <= 8:
                raise BenchmarkError("Query nesting exceeds budget")
        if depth:
            raise BenchmarkError("Unbalanced query grouping")
        _integer(case["limit"], 1, 50)
        _integer(case["fuzzy"], 0, 2)
        if case["fuzzy"] and '"' in text:
            raise BenchmarkError("Fuzzy phrase queries are unsupported")
        if case["workload"] is not None and (
            not isinstance(case["workload"], str)
            or not re.fullmatch(r"[a-z0-9-]{1,80}", case["workload"])
        ):
            raise BenchmarkError("Invalid workload")
        _text(case["grounding"])
        expected = _object(case["expected"], {"kind", "value"})
        if expected["kind"] == "source_path":
            _source_path(expected["value"])
        elif expected["kind"] == "source_prefix":
            prefix = _text(expected["value"])
            _source_path(prefix + "article.md")
            if not prefix.endswith("/") or prefix == "docs/":
                raise BenchmarkError("Expected a workload/article directory prefix")
        elif expected["kind"] != "no_hits" or expected["value"] is not None:
            raise BenchmarkError("Invalid expected-source judgment")
    return cases


def judge(paths: list[str], expected: dict[str, Any]) -> bool:
    """Judge top-limit source membership, never learn an expectation from results."""
    if expected["kind"] == "no_hits":
        return not paths
    if expected["kind"] == "source_path":
        return expected["value"] in paths
    return any(path.startswith(expected["value"]) for path in paths)


def _absolute_file(path: Path) -> Path:
    if not path.is_absolute() or not path.is_file():
        raise BenchmarkError("All inputs must name absolute existing files")
    return path.resolve(strict=True)


def isolated_environment(home: Path) -> dict[str, str]:
    """Allow only OS necessities and explicit isolated profile/config/temp locations."""
    environment = {
        "PATH": "",
        "HOME": str(home),
        "USERPROFILE": str(home),
        "APPDATA": str(home / "config"),
        "LOCALAPPDATA": str(home / "local"),
        "XDG_CONFIG_HOME": str(home / "config"),
        "XDG_CACHE_HOME": str(home / "cache"),
        "XDG_STATE_HOME": str(home / "state"),
        "FAB_RUST_CONFIG_ROOT": str(home / "config" / "fab-rs"),
        "TMPDIR": str(home),
        "TMP": str(home),
        "TEMP": str(home),
        "LC_ALL": "C",
        "LANG": "C",
        "TERM": "dumb",
        "NO_COLOR": "1",
        "COLUMNS": "100",
        "LINES": "24",
    }
    if os.name == "nt":
        for key in ("SystemRoot", "WINDIR"):
            if key in os.environ:
                environment[key] = os.environ[key]
    return environment


def invoke(
    executable: Path, arguments: list[str], timeout: float, home: Path
) -> tuple[dict[str, Any], bytes]:
    """Measure one trusted offline child; bound time/output and reap on all paths.

    Output is spooled under the workspace, never an ambient temporary directory.
    A watchdog checks the combined spool budget every 10 ms; this is not an OS
    filesystem quota or a network sandbox. No shell or inherited PATH is used.
    """
    failure = None
    exit_code = None
    process: subprocess.Popen[bytes] | None = None
    done = threading.Event()
    reason: list[str] = []
    stdout_path, stderr_path = home / "stdout.bin", home / "stderr.bin"

    def terminate() -> None:
        if process is not None and process.poll() is None:
            try:
                if os.name == "posix":
                    os.killpg(process.pid, signal.SIGKILL)
                else:
                    process.kill()
            except ProcessLookupError:
                pass

    def watch() -> None:
        expires = time.monotonic() + timeout
        while not done.wait(min(0.01, max(0.001, expires - time.monotonic()))):
            if time.monotonic() >= expires:
                reason.append("timeout")
            elif stdout_path.stat().st_size + stderr_path.stat().st_size > MAX_OUTPUT_BYTES:
                reason.append("output_limit")
            else:
                continue
            terminate()
            return

    with stdout_path.open("w+b") as stdout, stderr_path.open("w+b") as stderr:
        watchdog = threading.Thread(target=watch, daemon=True)
        watchdog.start()
        started = time.perf_counter_ns()
        try:
            process = subprocess.Popen(
                [str(executable), *arguments],
                stdin=subprocess.DEVNULL,
                stdout=stdout,
                stderr=stderr,
                cwd=home,
                env=isolated_environment(home),
                start_new_session=os.name == "posix",
            )
            if reason:
                terminate()
            exit_code = process.wait()
        except OSError:
            failure = "process_start_failed"
        finally:
            # Exclude watchdog shutdown, stream reads, hashing and validation.
            wall_ms = (time.perf_counter_ns() - started) / 1_000_000
            done.set()
            watchdog.join()
            terminate()
            if process is not None:
                process.wait()
        stdout_bytes, stderr_bytes = stdout_path.stat().st_size, stderr_path.stat().st_size
        stdout.seek(0)
        stderr.seek(0)
        raw = stdout.read(MAX_OUTPUT_BYTES + 1)
        errors = stderr.read(8192)
    if reason:
        failure = reason[0]
    if stdout_bytes + stderr_bytes > MAX_OUTPUT_BYTES:
        failure = "output_limit"
    return {
        "wall_ms": wall_ms,
        "exit_code": exit_code,
        "stdout_bytes": stdout_bytes,
        "stderr_bytes": stderr_bytes,
        "stdout_sha256": hashlib.sha256(raw).hexdigest() if len(raw) == stdout_bytes else None,
        "stderr_preview": errors.decode("utf-8", errors="replace"),
        "stderr_truncated": stderr_bytes > len(errors),
        "failure": failure,
    }, raw


def validate_manifest(value: Any) -> dict[str, Any]:
    """Keep actual index bytes/section counts distinct from raw Markdown metrics."""
    manifest = _object(value)
    if (
        manifest.get("source_commit") != SOURCE_COMMIT
        or manifest.get("schema_id") not in ("fabric-docs-sections-v1", "fabric-docs-sections-v2")
        or type(manifest.get("schema_version")) is not int
        or manifest["schema_version"] != 1
    ):
        raise BenchmarkError("Incompatible embedded corpus identity")
    if manifest["schema_id"] == "fabric-docs-sections-v2":
        features = _object(manifest.get("features"), {"stemmed", "metadata", "hierarchy", "autocomplete"})
        if any(type(value) is not bool for value in features.values()):
            raise BenchmarkError("Invalid rich-index capability profile")
        catalog = _object(manifest.get("filter_catalog"))
        for field, values in catalog.items():
            _text(field)
            for value, count in _object(values).items():
                _text(value)
                _integer(count, 1, 2**64 - 1)
    _integer(manifest.get("documents"), 1, 2**64 - 1)
    _integer(manifest.get("source_markdown_files"), 1, 2**64 - 1)
    _integer(manifest.get("source_markdown_bytes"), 1, 2**64 - 1)
    _text(manifest.get("compression"))
    files = manifest.get("files")
    if not isinstance(files, list) or not 1 <= len(files) <= 8192:
        raise BenchmarkError("Missing or excessive index component inventory")
    names = set()
    for component in files:
        _object(component, {"name", "bytes", "sha256"})
        name = _text(component["name"])
        if name in names or "/" in name or "\\" in name or name in (".", ".."):
            raise BenchmarkError("Invalid or duplicate component name")
        names.add(name)
        _integer(component["bytes"], 1, 2**64 - 1)
        _digest(component["sha256"], 64)
    return manifest


def _timings(value: Any) -> dict[str, int]:
    timings = _object(value, set(STAGES))
    for duration in timings.values():
        _duration(duration)
    if sum(timings[stage] for stage in STAGES[:-1]) > timings["total_us"]:
        raise BenchmarkError("Stage timings exceed total duration")
    return timings


def _hits(value: Any, case: dict[str, Any]) -> list[str]:
    if not isinstance(value, list) or len(value) > case["limit"]:
        raise BenchmarkError("Invalid or excessive hit list")
    paths = []
    ids = set()
    for hit in value:
        _object(hit)
        for key in ("id", "url"):
            if len(_text(hit.get(key))) > 4096:
                raise BenchmarkError("Excessive hit identity text")
        for key in ("title", "heading", "snippet"):
            if not isinstance(hit.get(key), str) or len(hit[key]) > (320 if key == "snippet" else 4096):
                raise BenchmarkError("Invalid hit text fields")
        _finite(hit.get("score"))
        path = _source_path(hit.get("source_path"))
        if case["workload"] and not path.startswith(f"docs/{case['workload']}/"):
            raise BenchmarkError("Hit escapes the requested workload")
        if hit["id"] in ids:
            raise BenchmarkError("Duplicate result section id")
        ids.add(hit["id"])
        paths.append(path)
    return paths


def _envelope(value: Any) -> dict[str, Any]:
    payload = _object(value)
    if payload.get("status") != "succeeded" or payload.get("source") != "embeddedFabricDocs":
        raise BenchmarkError("Invalid CLI success envelope")
    return payload


def validate_search(
    payload: Any, case: dict[str, Any], manifest: dict[str, Any]
) -> bool:
    """Check actual CLI result shape/provenance, then independently judge relevance."""
    payload = _envelope(payload)
    meta = _object(payload.get("meta"))
    if (
        meta.get("sourceCommit") != SOURCE_COMMIT
        or type(meta.get("indexedSections")) is not int
        or meta["indexedSections"] != manifest["documents"]
        or type(meta.get("resultLimit")) is not int
        or meta["resultLimit"] != case["limit"]
    ):
        raise BenchmarkError("Search metadata differs from corpus/options")
    _duration(meta.get("openUs"))
    _timings(meta.get("timings"))
    return judge(_hits(payload.get("data"), case), case["expected"])


def validate_variant_probe(
    target: str, observation: dict[str, Any], stdout: bytes
) -> None:
    """Verify independently pinned error states without accepting target-derived oracles."""
    expected_exit = {"baseline": 2, "engine_only": 1}[target]
    if observation["exit_code"] != expected_exit:
        raise BenchmarkError(f"{target} does not have the required variant exit state")
    if stdout or observation["stdout_bytes"] != 0:
        raise BenchmarkError(f"{target} error probe must have empty stdout")
    if observation["stderr_truncated"] or observation["stderr_bytes"] == 0:
        raise BenchmarkError(f"{target} error probe must have complete sanitized stderr")
    actual = decode_json(observation["stderr_preview"].encode("utf-8"))
    if actual != VARIANT_ERRORS[target]:
        raise BenchmarkError(f"{target} error probe violates the independent sanitized contract")
    observation["error_output"] = actual


def validate_reader(
    value: Any, cases: list[dict[str, Any]], iterations: int,
    mode: str, manifest: dict[str, Any],
) -> bool:
    """Recompute reader judgments and distributions from raw reported samples."""
    report = _object(value)
    if (
        type(report.get("schema_version")) is not int
        or report["schema_version"] != 1
        or report.get("units") != "microseconds"
        or report.get("mode") != mode
        or report.get("source_commit") != SOURCE_COMMIT
        or type(report.get("iterations")) is not int
        or report["iterations"] != iterations
        or canonical_digest(validate_manifest(report.get("manifest"))) != canonical_digest(manifest)
    ):
        raise BenchmarkError("Invalid reader report identity/options")
    _duration(report.get("open_us"))
    rows = report.get("cases")
    if not isinstance(rows, list) or len(rows) != len(cases):
        raise BenchmarkError("Incomplete reader case coverage")
    all_matched = True
    for case, row in zip(cases, rows):
        _object(row)
        for key in ("id", "text", "limit", "workload", "fuzzy", "expected", "grounding"):
            if row.get(key) != case[key]:
                raise BenchmarkError("Reader cases differ from supplied independent fixture")
        first_paths = _hits(row.get("observed_top_hits"), case)
        warm_paths = _hits(row.get("warmup_top_hits"), case)
        samples = row.get("samples")
        if not isinstance(samples, list) or len(samples) != iterations:
            raise BenchmarkError("Wrong reader sample count")
        warmup = _object(row.get("warmup"))
        for index, sample in enumerate([warmup, *samples]):
            _object(sample)
            _duration(sample.get("wall_us"))
            _timings(sample.get("timings"))
            paths = sample.get("observed_source_paths")
            ids = sample.get("observed_ids")
            if (
                not isinstance(paths, list) or len(paths) > case["limit"]
                or not isinstance(ids, list) or len(ids) != len(paths)
                or any(not isinstance(item, str) or not item for item in ids)
                or len(set(ids)) != len(ids)
            ):
                raise BenchmarkError("Invalid reader observed hit identities")
            for path in paths:
                _source_path(path)
                if case["workload"] and not path.startswith(f"docs/{case['workload']}/"):
                    raise BenchmarkError("Reader hit escapes requested workload")
            expected_hits = row["warmup_top_hits"] if index == 0 else row["observed_top_hits"]
            if index < 2 and (
                paths != (warm_paths if index == 0 else first_paths)
                or ids != [hit["id"] for hit in expected_hits]
            ):
                raise BenchmarkError("Reader hit evidence differs from sample")
            matched = judge(paths, case["expected"])
            if type(sample.get("expected_source_hit")) is not bool or sample["expected_source_hit"] != matched:
                raise BenchmarkError("Reader relevance judgment disagrees with source evidence")
            all_matched &= matched
        distributions = _object(
            row.get("distributions_us"), {"wall", "parse", "search", "fetch", "total"}
        )
        for key in distributions:
            values = [
                sample["wall_us"] if key == "wall" else sample["timings"][key + "_us"]
                for sample in samples
            ]
            if distributions[key] != summary(values):
                raise BenchmarkError("Reader distribution disagrees with raw samples")
    expected_status = "passed" if all_matched else "failed_relevance"
    if report.get("status") != expected_status:
        raise BenchmarkError("Reader status disagrees with relevance evidence")
    return all_matched


def _targets(args: argparse.Namespace, receipt: dict[str, Any]) -> dict[str, Path]:
    targets = {
        "baseline": _absolute_file(args.baseline),
        "engine_only": _absolute_file(args.engine_only),
        "embedded": _absolute_file(args.embedded),
    }
    if args.reader_benchmark:
        targets["reader"] = _absolute_file(args.reader_benchmark)
    elif args.ram_control:
        raise BenchmarkError("--ram-control requires --reader-benchmark; CLI has no --copy")
    metadata, digest = read_json(_absolute_file(args.build_metadata), MAX_FIXTURE_BYTES)
    _object(
        metadata,
        {"schema_version", "toolchain", "target_triple", "profile", "receipt", "variants"},
    )
    if type(metadata["schema_version"]) is not int or metadata["schema_version"] != 1 or metadata["profile"] != "release":
        raise BenchmarkError("Matched release-build metadata schema 1 required")
    for key in ("toolchain", "target_triple", "receipt"):
        _text(metadata[key])
    variants = _object(metadata["variants"], set(targets))
    _digest(args.source_sha, 40)
    _text(args.execution_grant)
    receipt["build_metadata"] = metadata
    receipt["build_metadata_sha256"] = digest
    receipt["artifacts_before"] = {}
    for name, path in targets.items():
        entry = _object(variants[name], {"source_sha", "sha256", "features"})
        _digest(entry["source_sha"], 40)
        _digest(entry["sha256"], 64)
        if (
            not isinstance(entry["features"], list)
            or any(not isinstance(feature, str) or not feature.strip() for feature in entry["features"])
        ):
            raise BenchmarkError("Features must be an explicit list of names")
        if entry["source_sha"] != args.source_sha:
            raise BenchmarkError("Every variant source SHA must match the requested source")
        has_docs = "docs-search" in entry["features"]
        if name == "baseline" and has_docs or name in ("engine_only", "embedded") and not has_docs:
            raise BenchmarkError("Declared features do not match the requested docs-search variant")
        if not os.access(path, os.X_OK):
            raise BenchmarkError("Target is not executable")
        observed = artifact(path)
        receipt["artifacts_before"][name] = observed
        if observed["sha256"] != entry["sha256"]:
            raise BenchmarkError("Executable digest differs from build/correctness receipt")
    feature_sets = {
        name: set(variants[name]["features"])
        for name in ("baseline", "engine_only", "embedded")
    }
    if (
        feature_sets["engine_only"] != feature_sets["embedded"]
        or feature_sets["baseline"] != feature_sets["embedded"] - {"docs-search"}
    ):
        raise BenchmarkError("Matched CLI variants must differ only by docs-search and embedded corpus")
    hashes = {
        receipt["artifacts_before"][name]["sha256"]
        for name in ("baseline", "engine_only", "embedded")
    }
    if len(hashes) != 3:
        raise BenchmarkError("Incompatible CLI variants must have distinct executable hashes")
    return targets


def run(args: argparse.Namespace, receipt: dict[str, Any]) -> None:
    """Run finite serial interleaved subprocesses and a separately reported reader."""
    deadline = time.monotonic() + args.deadline_seconds
    targets = _targets(args, receipt)
    fixture, fixture_digest = read_json(_absolute_file(args.queries), MAX_FIXTURE_BYTES)
    cases = load_queries(fixture)
    receipt["query_fixture"] = fixture
    receipt["query_fixture_sha256"] = fixture_digest
    if args.manifest_sha256 is not None:
        _digest(args.manifest_sha256, 64)
    root = Path.cwd() / (".docs-search-run-" + uuid.uuid4().hex)
    root.mkdir()
    observations: list[dict[str, Any]] = receipt["observations"]
    randomizer = random.Random(SEED)
    relevance = True
    try:
        homes = {name: root / name for name in targets}
        for home in homes.values():
            home.mkdir()
            for child in ("config", "cache", "state", "local"):
                (home / child).mkdir()

        def execute(
            target: str, arguments: list[str], kind: str, phase: str, index: int,
            case_id: str | None = None,
        ) -> tuple[dict[str, Any], bytes]:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise BenchmarkError("Overall deadline exhausted; comparison incomplete")
            row, raw = invoke(
                targets[target], arguments,
                min(args.process_timeout_seconds, remaining), homes[target],
            )
            row.update(target=target, arguments=arguments, kind=kind, phase=phase, index=index, case_id=case_id)
            row["valid"] = False
            row["stdout_preview"] = raw[:8192].decode("utf-8", errors="replace")
            row["stdout_truncated"] = len(raw) > 8192
            observations.append(row)
            if kind in ("query", "corpus", "reader"):
                try:
                    row["output"] = decode_json(raw)
                except (ValueError, UnicodeError, RecursionError):
                    row["failure"] = row["failure"] or "invalid_json_stdout"
            if row["failure"] is None and kind == "variant_probe":
                try:
                    validate_variant_probe(target, row, raw)
                except (ValueError, UnicodeError, RecursionError):
                    row["failure"] = "variant_state_mismatch"
                    raise
            if row["failure"] is None and kind != "variant_probe" and row["stderr_bytes"]:
                row["failure"] = "unexpected_stderr"
            if row["failure"] is None and kind != "variant_probe" and row["exit_code"] != 0:
                # Preserve legitimate false-relevance reports, but never arbitrary exit 1.
                if not (
                    kind == "reader" and row["exit_code"] == 1
                    and isinstance(row.get("output"), dict)
                    and row["output"].get("status") == "failed_relevance"
                ):
                    row["failure"] = "nonzero_exit"
            if row["failure"]:
                raise BenchmarkError(f"{target} {kind}: {row['failure']}")
            if time.monotonic() > deadline:
                row["failure"] = "overall_deadline"
                raise BenchmarkError("Overall deadline exhausted; comparison incomplete")
            return row, raw

        probe_command = ["search", "docs", "--corpus-info", "--format", "json"]
        for name in ("baseline", "engine_only"):
            row, _ = execute(name, probe_command, "variant_probe", "identity", 0)
            row["valid"] = True
            row.pop("stdout_preview")
            row.pop("stdout_truncated")
        row, _ = execute("embedded", probe_command, "corpus", "identity", 0)
        manifest = validate_manifest(_envelope(row["output"]).get("data"))
        digest = canonical_digest(manifest)
        receipt["manifest_sha256"] = digest
        if args.manifest_sha256 is not None and digest != args.manifest_sha256:
            raise BenchmarkError("Manifest differs from requested canonical digest")
        receipt["index_component_bytes"] = sum(component["bytes"] for component in manifest["files"])
        row["valid"] = True
        row.pop("stdout_preview")
        row.pop("stdout_truncated")
        receipt["variant_states"] = {
            "baseline": "docs_search_command_absent",
            "engine_only": "docs_search_engine_without_corpus",
            "embedded": "embedded_corpus_verified",
        }
        before = receipt["artifacts_before"]
        receipt["executable_byte_deltas"] = {
            "engine_only_minus_baseline": before["engine_only"]["bytes"] - before["baseline"]["bytes"],
            "embedded_minus_engine_only": before["embedded"]["bytes"] - before["engine_only"]["bytes"],
            "embedded_minus_baseline": before["embedded"]["bytes"] - before["baseline"]["bytes"],
        }
        for phase, count in (("first_observed", 1), ("warmup", args.warmups), ("sample", args.samples)):
            for index in range(count):
                order = [(name, flag) for name in targets if name != "reader" for flag in ("--help", "--version")]
                randomizer.shuffle(order)
                for name, flag in order:
                    row, raw = execute(name, [flag], "startup", phase, index)
                    if flag == "--version":
                        valid = raw == VERSION
                    else:
                        text = raw.decode("utf-8")
                        valid = bool(text.strip()) and all(token in text for token in ("Usage:", "fab-rs", "--help", "--version"))
                    if not valid:
                        row["failure"] = "invalid_stdout_contract"
                        raise BenchmarkError("Startup output violates independent contract")
                    row["valid"] = True
                    row.pop("stdout_preview")
                    row.pop("stdout_truncated")
        for phase, count in (("first_observed", 1), ("warmup", args.warmups), ("sample", args.samples)):
            for index in range(count):
                order = list(cases)
                randomizer.shuffle(order)
                for case in order:
                    command = [
                        "search", "docs", "--text", case["text"], "--limit", str(case["limit"]),
                        "--fuzzy", str(case["fuzzy"]), "--format", "json",
                    ]
                    if case["workload"] is not None:
                        command.extend(["--workload", case["workload"]])
                    row, _ = execute("embedded", command, "query", phase, index, case["id"])
                    matched = validate_search(row["output"], case, manifest)
                    row["expected"] = case["expected"]
                    row["expected_source_hit"] = matched
                    relevance &= matched
                    row["valid"] = True
                    row.pop("stdout_preview")
                    row.pop("stdout_truncated")
        receipt["reader_reports"] = {}
        if "reader" in targets:
            for mode in (["embedded", "copied_ram"] if args.ram_control else ["embedded"]):
                command = ["--iterations", str(args.iterations), "--queries", str(args.queries)]
                if mode == "copied_ram":
                    command.append("--copy")
                row, _ = execute("reader", command, "reader", mode, 0)
                matched = validate_reader(row["output"], cases, args.iterations, mode, manifest)
                if row["exit_code"] != (0 if matched else 1):
                    row["failure"] = "reader_exit_status_mismatch"
                    raise BenchmarkError("Reader exit disagrees with relevance status")
                row["valid"] = True
                relevance &= matched
                receipt["reader_reports"][mode] = row["output"]
                row["output"] = {"reader_report_ref": mode}
                row.pop("stdout_preview")
                row.pop("stdout_truncated")
        receipt["artifacts_after"] = {name: artifact(path) for name, path in targets.items()}
        if receipt["artifacts_after"] != receipt["artifacts_before"]:
            raise BenchmarkError("Executable changed during measurement")
        if time.monotonic() > deadline:
            raise BenchmarkError("Overall deadline exhausted; comparison incomplete")
        receipt["startup_summaries"] = [
            {
                "target": name, "flag": flag,
                "wall_ms": summary([
                    row["wall_ms"] for row in observations
                    if row["target"] == name and row["kind"] == "startup"
                    and row["phase"] == "sample" and row["arguments"] == [flag]
                ]),
            }
            for name in targets if name != "reader" for flag in ("--help", "--version")
        ]
        receipt["query_summaries"] = []
        for case in cases:
            rows = [
                row for row in observations if row["kind"] == "query"
                and row["phase"] == "sample" and row["case_id"] == case["id"]
            ]
            receipt["query_summaries"].append({
                "case_id": case["id"], "wall_ms": summary([row["wall_ms"] for row in rows]),
                "open_us": summary([row["output"]["meta"]["openUs"] for row in rows]),
                "timings_us": {
                    stage: summary([row["output"]["meta"]["timings"][stage] for row in rows])
                    for stage in STAGES
                },
                "expected": case["expected"],
                "all_measured_expected_source_hit": all(row["expected_source_hit"] for row in rows),
            })
        receipt["status"] = "passed" if relevance else "failed_relevance"
    except (BenchmarkError, UnicodeError, ValueError, RecursionError):
        if observations and not observations[-1]["valid"] and observations[-1]["failure"] is None:
            observations[-1]["failure"] = "output_validation_failed"
        raise
    finally:
        if "artifacts_after" not in receipt:
            receipt["artifacts_after"] = {}
            for name, path in targets.items():
                try:
                    receipt["artifacts_after"][name] = artifact(path)
                except OSError as error:
                    receipt["artifacts_after"][name] = {"error": type(error).__name__}
        shutil.rmtree(root)


def _bounded_int(minimum: int, maximum: int) -> Any:
    def parse(value: str) -> int:
        try:
            return _integer(int(value), minimum, maximum)
        except (ValueError, OverflowError) as error:
            raise argparse.ArgumentTypeError(f"Expected integer {minimum}..{maximum}") from error
    return parse


def _bounded_seconds(maximum: float) -> Any:
    def parse(value: str) -> float:
        try:
            result = _finite(float(value))
            if not 0.1 <= result <= maximum:
                raise BenchmarkError("Time bound outside allowed range")
            return result
        except (ValueError, OverflowError) as error:
            raise argparse.ArgumentTypeError(f"Expected finite seconds 0.1..{maximum}") from error
    return parse


def main(argv: list[str] | None = None) -> int:
    """Reserve exclusive evidence before running; preserve failures in the receipt."""
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    for option in ("baseline", "engine-only", "embedded", "build-metadata", "output"):
        parser.add_argument("--" + option, type=Path, required=True)
    parser.add_argument("--reader-benchmark", type=Path)
    parser.add_argument("--ram-control", action="store_true")
    parser.add_argument("--queries", type=Path, default=DEFAULT_QUERIES)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--execution-grant", required=True)
    parser.add_argument("--manifest-sha256", help="SHA256 of sorted compact UTF-8 manifest JSON")
    parser.add_argument("--samples", type=_bounded_int(1, 200), default=30)
    parser.add_argument("--warmups", type=_bounded_int(1, 10), default=3)
    parser.add_argument("--iterations", type=_bounded_int(1, 1000), default=100)
    parser.add_argument("--process-timeout-seconds", type=_bounded_seconds(60), default=15)
    parser.add_argument("--deadline-seconds", type=_bounded_seconds(600), default=600)
    args = parser.parse_args(argv)
    if not args.output.is_absolute() or not args.output.parent.is_dir():
        raise BenchmarkError("Output must be an absolute new JSON file in an existing directory")
    receipt: dict[str, Any] = {
        "schema_version": 1, "benchmark": "embedded-fabric-docs", "status": "failed",
        "started_utc": datetime.now(timezone.utc).isoformat(),
        "harness": artifact(Path(__file__).resolve()),
        "parameters": {key: str(value) if isinstance(value, Path) else value for key, value in vars(args).items()},
        "machine": {
            "platform": platform.platform(), "os_release": platform.release(),
            "architecture": platform.machine(), "processor": platform.processor() or None,
            "logical_cpus": os.cpu_count(), "python": sys.version,
            "load_average": list(os.getloadavg()) if hasattr(os, "getloadavg") else None,
        },
        "cache_classification": "process-cold; OS-cache-uncontrolled; no cache eviction attempted",
        "traffic_classification": "corpus-grounded-domain-probes-not-user-traffic",
        "startup_help_contract": "structural-only; no byte-parity assertion or learned snapshot",
        "startup_version_contract_hex": VERSION.hex(),
        "network_enforcement": "offline invocation allowlist, not an OS network sandbox",
        "units": {"wall": "milliseconds", "library_stages_and_open": "microseconds", "size": "bytes"},
        "shuffle_seed": SEED, "observations": [],
    }
    with args.output.open("x", encoding="utf-8") as output:
        try:
            run(args, receipt)
        except (BenchmarkError, OSError, UnicodeError, ValueError, RecursionError) as error:
            receipt["status"] = "failed"
            receipt["error"] = str(error) if isinstance(error, BenchmarkError) else type(error).__name__
        finally:
            receipt["finished_utc"] = datetime.now(timezone.utc).isoformat()
            json.dump(receipt, output, indent=2, allow_nan=False)
            output.write("\n")
    return 0 if receipt["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
