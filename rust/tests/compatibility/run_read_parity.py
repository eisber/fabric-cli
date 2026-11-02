# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Require actual native read/query observations and independent source guards."""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
WORKSPACE = HERE.parents[1]
TIMEOUT_SECONDS = 300


def run(
    read_probe: Path, query_probe: Path, folder_probe: Path, artifacts: Path
) -> None:
    """Execute every comparison, stopping on any failed or timed-out process.

    Probes must already be built. This runner neither installs dependencies nor
    rewrites fixtures, selects historical exceptions, or configures credentials.
    Each oracle owns its independent expectations and exact/unsupported counts.
    """
    read_probe = read_probe.resolve(strict=True)
    query_probe = query_probe.resolve(strict=True)
    folder_probe = folder_probe.resolve(strict=True)
    artifacts.mkdir(parents=True, exist_ok=True)
    observations = artifacts.resolve() / "query-observations.json"
    environment = {
        **os.environ,
        "PYTHONUTF8": "1",
        "PYTHONDONTWRITEBYTECODE": "1",
    }

    def python(*arguments: str | Path) -> None:
        subprocess.run(
            [sys.executable, "-B", *(str(argument) for argument in arguments)],
            cwd=WORKSPACE,
            env=environment,
            check=True,
            timeout=TIMEOUT_SECONDS,
        )

    for reference in ("reference.py", "metadata_reference.py"):
        python(
            HERE / "commands" / reference,
            "--request-mode",
            "source-routed",
            "--rust-probe",
            read_probe,
        )
    python(HERE / "commands" / "specialized_reference.py", "--rust-probe", read_probe)
    python(
        "-m",
        "unittest",
        "discover",
        "-s",
        HERE / "commands",
        "-p",
        "test_*.py",
        "-v",
    )
    with observations.open("wb") as output:
        subprocess.run(
            [str(query_probe)],
            cwd=WORKSPACE,
            env=environment,
            stdout=output,
            check=True,
            timeout=TIMEOUT_SECONDS,
        )
    python(HERE / "query" / "reference.py", "--observations", observations)
    python(
        "-m",
        "unittest",
        "discover",
        "-s",
        HERE / "query",
        "-p",
        "test_reference.py",
        "-v",
    )
    python(HERE / "folder_commands" / "reference.py", "--rust-probe", folder_probe)
    python(
        "-m",
        "unittest",
        "discover",
        "-s",
        HERE / "folder_commands",
        "-p",
        "test_reference.py",
        "-v",
    )
    python(
        HERE / "query_arguments" / "reference.py",
        "--rust-probe",
        read_probe,
        "--folder-probe",
        folder_probe,
    )
    python(
        "-m",
        "unittest",
        "discover",
        "-s",
        HERE / "query_arguments",
        "-p",
        "test_reference.py",
        "-v",
    )
    python(HERE / "workspace_get" / "reference.py")
    python(
        "-m",
        "unittest",
        "discover",
        "-s",
        HERE / "workspace_get",
        "-p",
        "test_reference.py",
        "-v",
    )
    for probe in (read_probe, folder_probe):
        python(HERE / "workspace_get" / "consumer_reference.py", "--rust-probe", probe)
    python(
        "-m",
        "unittest",
        "discover",
        "-s",
        HERE / "workspace_get",
        "-p",
        "test_consumer_reference.py",
        "-v",
    )


def main() -> None:
    """Run the current contract, never a successful-looking historical fallback."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--read-probe", type=Path, required=True)
    parser.add_argument("--query-probe", type=Path, required=True)
    parser.add_argument("--folder-probe", type=Path, required=True)
    parser.add_argument("--artifacts", type=Path, required=True)
    arguments = parser.parse_args()
    for probe in (arguments.read_probe, arguments.query_probe, arguments.folder_probe):
        if not probe.is_file():
            parser.error(f"Native probe does not exist: {probe}")
    run(
        arguments.read_probe,
        arguments.query_probe,
        arguments.folder_probe,
        arguments.artifacts,
    )


if __name__ == "__main__":
    main()
