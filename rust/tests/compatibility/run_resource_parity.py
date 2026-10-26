# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Require every released resource comparison with native, already-built probes."""

from __future__ import annotations

import argparse
import io
import os
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path, PurePosixPath

HERE = Path(__file__).resolve().parent
WORKSPACE = HERE.parents[1]
REPOSITORY = WORKSPACE.parent
SOURCE = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
PAYLOADS = "src/fabric_cli/commands/fs/payloads"
TIMEOUT_SECONDS = 300
COMPARISONS = (
    ("fabric-cli-mutations", "python_reference.py", "offline_mutation", False),
    ("fabric-cli-mutations", "operation_reference.py", "python_operation", False),
    ("fabric-cli-mutations", "error_reference.py", "error_contract", False),
    ("fabric-cli-mutations", "onelake_reference.py", "offline_onelake", False),
    ("fabric-cli-mutations", "onelake_write_reference.py", "offline_onelake_write", False),
    ("fabric-cli-mutations", "shortcut_reference.py", "offline_shortcut", False),
    ("fabric-cli-mutations", "removal_reference.py", "offline_removal", False),
    ("fabric-cli-mutations", "public_set_reference.py", "public_set", False),
    ("fabric-cli-mutations", "copy_preflight_reference.py", "offline_copy_preflight", False),
    ("fabric-cli-mutations", "metadata_set_reference.py", "offline_mutation", False),
    ("fabric-cli-mutations", "creation_reference.py", "offline_mutation", True),
    ("fabric-cli-mutations", "dynamic_creation_reference.py", "offline_mutation", True),
    ("fabric-cli-mutations", "creation_policy_reference.py", "offline_mutation", True),
    ("fabric-cli-mutations", "creation_value_reference.py", "offline_mutation", True),
    ("fabric-sdk-operations", "continuation_reference.py", "continuation_query", False),
)


def run(probes: Path, artifacts: Path) -> None:
    """Fail closed on missing probes, source exports, comparisons or timeouts.

    Only immutable development-time template inputs are exported; current Python
    source, old fixtures and expected outputs are never rewritten. Both Windows
    executable suffixes and extensionless native probes use this same runner.
    """
    probes = probes.resolve(strict=True)
    suffix = ".exe" if os.name == "nt" else ""
    executables = {
        name: (probes / f"{name}{suffix}").resolve(strict=True)
        for _, _, name, _ in COMPARISONS
    }
    if not all(probe.is_file() for probe in executables.values()):
        raise FileNotFoundError("Every resource probe must be a regular file")
    environment = {
        **os.environ,
        "PYTHONUTF8": "1",
        "PYTHONDONTWRITEBYTECODE": "1",
    }
    artifacts.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="resource-source-", dir=artifacts) as directory:
        exported = Path(directory)
        archive = subprocess.run(
            ["git", "archive", "--format=zip", SOURCE, PAYLOADS],
            cwd=REPOSITORY,
            env=environment,
            stdout=subprocess.PIPE,
            check=True,
            timeout=TIMEOUT_SECONDS,
        )
        with zipfile.ZipFile(io.BytesIO(archive.stdout)) as payloads:
            members = payloads.namelist()
            if not members or not any(not name.endswith("/") for name in members):
                raise ValueError("Pinned template archive is empty")
            for name in members:
                path = PurePosixPath(name)
                if (
                    path.is_absolute()
                    or ".." in path.parts
                    or "\\" in name
                    or not (
                        name.endswith("/") and (PAYLOADS + "/").startswith(name)
                        or name.startswith(PAYLOADS + "/")
                    )
                ):
                    raise ValueError("Pinned template archive contains an unexpected path")
            payloads.extractall(exported)
        for package, reference, name, needs_source in COMPARISONS:
            arguments = [
                sys.executable,
                "-B",
                str(WORKSPACE / "crates" / package / "tests" / reference),
                "--probe",
                str(executables[name]),
            ]
            if needs_source:
                arguments.extend(("--source-export", str(exported)))
            subprocess.run(
                arguments,
                cwd=WORKSPACE,
                env=environment,
                check=True,
                timeout=TIMEOUT_SECONDS,
            )


def main() -> None:
    """Run all fifteen source comparisons without installing or changing inputs."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--probes", type=Path, required=True)
    parser.add_argument("--artifacts", type=Path, required=True)
    arguments = parser.parse_args()
    run(arguments.probes, arguments.artifacts)


if __name__ == "__main__":
    main()
