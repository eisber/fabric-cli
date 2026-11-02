# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Prepare frozen Python oracles without checking out or building old Rust code.

``prepare --candidate-root ABS --output ABS`` creates a new artifact directory
containing ``source`` (the isolated checkout) and ``manifest.json``. Its JSON
receipt also exposes ``checkout`` and ``rust_root``. Run copied Python oracles
there with absolute paths to native probes built in the original candidate.
``verify --output ABS`` checks the checkout and its byte-identical overlays.

The unchanged resource oracle reads native templates at their pre-relocation
path. Only in this artifact, explicit evidence aliases supply those candidate
template bytes; every alias records and verifies its candidate source. No Rust
implementation or build output is copied to satisfy that historical path.

The existing archive-only template export cannot satisfy references which also
verify HEAD and historical Git objects. A local shared clone preserves those
checks without registering worktrees or changing the candidate repository.
Artifacts are never reused or recursively removed, including after a failure.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import subprocess
import zipfile
from pathlib import Path, PurePosixPath
from typing import Any

SOURCE_COMMIT = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
SCHEMA_VERSION = 2
TIMEOUT_SECONDS = 120
EXCLUDED_PARTS = {".git", "target", "__pycache__", ".venv", "venv"}
EVIDENCE_DIRECTORIES = {
    PurePosixPath("rust/crates/fabric-cli-mutations/templates"): PurePosixPath(
        "rust/capabilities/resources/fabric-cli-mutations/templates"
    ),
}


def _git(root: Path, *arguments: str) -> bytes:
    environment = {
        key: value for key, value in os.environ.items() if not key.startswith("GIT_")
    }
    environment["GIT_TERMINAL_PROMPT"] = "0"
    return subprocess.check_output(
        [
            "git",
            "--no-pager",
            "-c",
            "core.autocrlf=false",
            "-c",
            f"core.hooksPath={root / '.git' / 'pinned-source-disabled-hooks'}",
            *arguments,
        ],
        cwd=root,
        env=environment,
        stderr=subprocess.PIPE,
        timeout=TIMEOUT_SECONDS,
    )


def _absolute(path: Path) -> Path:
    if not path.is_absolute():
        raise ValueError("Candidate and artifact paths must be absolute")
    return path.resolve()


def _pin(root: Path, source_commit: str) -> None:
    if source_commit != SOURCE_COMMIT:
        raise ValueError("Only the frozen Python source commit is permitted")
    try:
        actual = _git(root, "rev-parse", "--verify", f"{source_commit}^{{commit}}")
    except subprocess.CalledProcessError as error:
        raise ValueError(
            "Frozen Python source commit is unavailable locally"
        ) from error
    if actual.decode().strip() != SOURCE_COMMIT:
        raise ValueError("Frozen Python source commit did not resolve exactly")


def _relative(value: str) -> PurePosixPath:
    path = PurePosixPath(value)
    if (
        not value
        or path.is_absolute()
        or ".." in path.parts
        or "\\" in value
        or ":" in value
        or path.as_posix() != value
    ):
        raise ValueError(f"Unsafe overlay path: {value!r}")
    return path


def _is_overlay(value: str) -> bool:
    path = _relative(value)
    return (
        path.parts[0] == "rust"
        and not EXCLUDED_PARTS.intersection(path.parts)
        and (
            path.suffix == ".py"
            or path.name == "Cargo.toml"
            or bool({"tests", "fixtures", "templates"}.intersection(path.parts[1:-1]))
        )
    )


def overlay_paths(candidate: Path) -> list[str]:
    """Discover current oracle inputs independently of crate directory depth."""
    listed = _git(
        candidate,
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
        "rust",
    )
    result = []
    for value in sorted(set(listed.decode("utf-8").split("\0")) - {""}):
        if not _is_overlay(value):
            continue
        path = candidate.joinpath(*PurePosixPath(value).parts)
        if not path.exists():
            continue
        if (
            path.is_symlink()
            or not path.is_file()
            or not path.resolve().is_relative_to(candidate)
        ):
            raise ValueError(f"Oracle overlay is not a contained regular file: {value}")
        result.append(value)
    if not result:
        raise ValueError("Candidate contains no oracle scripts or fixtures")
    return result


def _overlay_sources(candidate: Path) -> dict[str, str]:
    sources = {value: value for value in overlay_paths(candidate)}
    for destination, source in EVIDENCE_DIRECTORIES.items():
        for value in list(sources):
            path = PurePosixPath(value)
            if not path.is_relative_to(source):
                continue
            alias = (destination / path.relative_to(source)).as_posix()
            if alias in sources:
                raise ValueError(
                    f"Candidate conflicts with oracle evidence alias: {alias}"
                )
            sources[alias] = value
    return sources


def _digest(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def _source_files(checkout: Path) -> dict[str, bytes]:
    archive = _git(checkout, "archive", "--format=zip", SOURCE_COMMIT, "src")
    with zipfile.ZipFile(io.BytesIO(archive)) as source:
        files = {
            entry.filename: source.read(entry)
            for entry in source.infolist()
            if not entry.is_dir()
        }
    if not files or any(_relative(value).parts[0] != "src" for value in files):
        raise ValueError("Frozen Python source archive is empty or invalid")
    return files


def _verify_source(checkout: Path) -> None:
    _pin(checkout, SOURCE_COMMIT)
    top = Path(
        _git(checkout, "rev-parse", "--show-toplevel").decode().strip()
    ).resolve()
    if top != checkout:
        raise ValueError("Oracle checkout is not an isolated repository")
    if _git(checkout, "rev-parse", "HEAD").decode().strip() != SOURCE_COMMIT:
        raise ValueError("Oracle HEAD differs from the frozen Python source commit")
    if _git(checkout, "rev-parse", "--symbolic-full-name", "HEAD").strip() != b"HEAD":
        raise ValueError("Oracle checkout must remain detached")
    expected = _source_files(checkout)
    actual = {
        path.relative_to(checkout).as_posix()
        for path in (checkout / "src").rglob("*")
        if path.is_file() or path.is_symlink()
    }
    if actual != expected.keys():
        raise ValueError("Historical Python source file inventory changed")
    for value, raw in expected.items():
        path = checkout.joinpath(*PurePosixPath(value).parts)
        if path.is_symlink() or path.read_bytes() != raw:
            raise ValueError(f"Historical Python source changed: {value}")


def _receipt(output: Path, manifest: dict[str, Any]) -> dict[str, Any]:
    return {
        "checkout": str(output / "source"),
        "rust_root": str(output / "source" / "rust"),
        "manifest": str(output / "manifest.json"),
        "source_commit": manifest["source_commit"],
        "candidate_commit": manifest["candidate_commit"],
        "overlay_file_count": len(manifest["overlays"]),
        "evidence_file_count": sum(
            value != record["source"] for value, record in manifest["overlays"].items()
        ),
    }


def prepare(
    candidate_root: Path, output: Path, source_commit: str = SOURCE_COMMIT
) -> dict[str, Any]:
    """Create a fresh, local-only oracle checkout; never overwrite or build it."""
    candidate = _absolute(candidate_root)
    output = _absolute(output)
    _pin(candidate, source_commit)
    top = Path(
        _git(candidate, "rev-parse", "--show-toplevel").decode().strip()
    ).resolve()
    if top != candidate:
        raise ValueError("Candidate root must be the repository root")
    sources = _overlay_sources(candidate)
    if output.exists():
        raise FileExistsError(
            "Artifact output already exists; choose a fresh directory"
        )
    if output.is_relative_to(candidate) and _is_overlay(
        (output / "canary.py").relative_to(candidate).as_posix()
    ):
        raise ValueError("Artifact output cannot be inside an oracle input directory")
    candidate_commit = _git(candidate, "rev-parse", "HEAD").decode().strip()
    output.mkdir(parents=True, exist_ok=False)
    checkout = output / "source"
    _git(
        candidate,
        "clone",
        "--local",
        "--shared",
        "--no-checkout",
        "--no-tags",
        "--",
        str(candidate),
        str(checkout),
    )
    _git(checkout, "checkout", "--detach", source_commit)
    overlays: dict[str, dict[str, str]] = {}
    for value, source in sources.items():
        raw = candidate.joinpath(*PurePosixPath(source).parts).read_bytes()
        target = checkout.joinpath(*PurePosixPath(value).parts)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(raw)
        overlays[value] = {"source": source, "sha256": _digest(raw)}
    manifest = {
        "schema_version": SCHEMA_VERSION,
        "source_commit": source_commit,
        "candidate_root": str(candidate),
        "candidate_commit": candidate_commit,
        "overlays": overlays,
    }
    (output / "manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return verify(output)


def verify(output: Path) -> dict[str, Any]:
    """Reject changed source, overlays, candidate identity, or missing inputs."""
    output = _absolute(output)
    manifest = json.loads((output / "manifest.json").read_text(encoding="utf-8"))
    if (
        manifest.get("schema_version") != SCHEMA_VERSION
        or manifest.get("source_commit") != SOURCE_COMMIT
    ):
        raise ValueError("Pinned-source artifact identity changed")
    candidate = _absolute(Path(manifest["candidate_root"]))
    checkout = output / "source"
    _verify_source(checkout)
    if (
        _git(candidate, "rev-parse", "HEAD").decode().strip()
        != manifest["candidate_commit"]
    ):
        raise ValueError(
            "Candidate revision changed while preparing or using the oracle"
        )
    sources = _overlay_sources(candidate)
    if sources.keys() != manifest["overlays"].keys():
        raise ValueError("Candidate oracle overlay inventory changed")
    actual = {
        path.relative_to(checkout).as_posix()
        for path in (checkout / "rust").rglob("*")
        if path.is_file() or path.is_symlink()
    }
    if actual != sources.keys():
        raise ValueError("Isolated oracle overlay inventory changed")
    for value, source in sources.items():
        original = candidate.joinpath(*PurePosixPath(source).parts)
        copied = checkout.joinpath(*PurePosixPath(value).parts)
        if (
            copied.is_symlink()
            or not copied.resolve().is_relative_to(checkout)
            or {"source": source, "sha256": _digest(original.read_bytes())}
            != manifest["overlays"][value]
            or copied.read_bytes() != original.read_bytes()
        ):
            raise ValueError(f"Oracle overlay changed: {value}")
    return _receipt(output, manifest)


def main() -> None:
    """Print a JSON receipt for preparation or fail-closed verification."""
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("prepare")
    create.add_argument("--candidate-root", required=True, type=Path)
    create.add_argument("--output", required=True, type=Path)
    create.add_argument("--source-commit", default=SOURCE_COMMIT)
    check = commands.add_parser("verify")
    check.add_argument("--output", required=True, type=Path)
    arguments = parser.parse_args()
    if arguments.command == "prepare":
        result = prepare(
            arguments.candidate_root, arguments.output, arguments.source_commit
        )
    else:
        result = verify(arguments.output)
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
