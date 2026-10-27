# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Prepare an offline Windows wheelhouse from pinned public source and wheels."""

import argparse
import email
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tempfile
import urllib.request
import zipfile

SOURCE = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
ROOT = Path(__file__).resolve().parents[2]
CONSTRAINTS = Path(__file__).with_name("rust-fast-windows-constraints.txt")
BUILD_REQUIREMENT = "setuptools==84.2.1"


def canonical(name: str) -> str:
    """Normalize a distribution name without importing product code."""
    return re.sub(r"[-_.]+", "-", name).lower()


def file_hash(path: Path) -> str:
    """Hash the complete artifact bytes."""
    return hashlib.sha256(path.read_bytes()).hexdigest()


def pins(text: str) -> dict[str, str]:
    """Reject unpinned, duplicate or executable requirements."""
    result: dict[str, str] = {}
    for line in text.splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        match = re.fullmatch(r"([A-Za-z0-9_.-]+)==([A-Za-z0-9_.+!-]+)", line)
        if not match or canonical(match[1]) in result:
            raise ValueError(f"Invalid or duplicate exact pin: {line}")
        result[canonical(match[1])] = match[2]
    return result


def wheel_identity(path: Path) -> tuple[str, str]:
    """Read the unique wheel metadata without installing or importing it."""
    with zipfile.ZipFile(path) as wheel:
        names = [
            name for name in wheel.namelist() if name.endswith(".dist-info/METADATA")
        ]
        if len(names) != 1:
            raise ValueError(f"Ambiguous wheel metadata: {path.name}")
        metadata = email.message_from_bytes(wheel.read(names[0]))
    name, version = metadata["Name"], metadata["Version"]
    if not name or not version:
        raise ValueError(f"Missing wheel identity: {path.name}")
    return canonical(name), version


def verify_public_wheel(path: Path, metadata: dict) -> dict:
    """Require a non-yanked PyPI filename, exact size and published SHA256."""
    entries = [
        item
        for item in metadata["urls"]
        if item["filename"] == path.name and not item["yanked"]
    ]
    actual = file_hash(path)
    if (
        len(entries) != 1
        or entries[0]["digests"]["sha256"] != actual
        or entries[0]["size"] != path.stat().st_size
        or not entries[0]["url"].startswith("https://files.pythonhosted.org/")
    ):
        raise ValueError(f"Public wheel provenance mismatch: {path.name}")
    return {
        "filename": path.name,
        "sha256": actual,
        "bytes": path.stat().st_size,
        "url": entries[0]["url"],
    }


def verify_product_wheel(path: Path, source: Path) -> int:
    """Compare every packaged product file with the exact archived source."""
    count = 0
    with zipfile.ZipFile(path) as wheel:
        packaged = {name for name in wheel.namelist() if name.startswith("fabric_cli/")}
        for name in packaged:
            original = source / "src" / name
            if not original.is_file() or wheel.read(name) != original.read_bytes():
                raise ValueError(f"Product/source mismatch: {name}")
            count += 1
        required = {
            path.relative_to(source / "src").as_posix()
            for path in (source / "src" / "fabric_cli").rglob("*")
            if path.is_file() and path.suffix in {".py", ".yaml", ".json", ".txt"}
        }
        if not count or required - packaged:
            raise ValueError(
                f"Missing packaged source files: {sorted(required - packaged)}"
            )
    return count


def run(args: list[str], cwd: Path, log: Path) -> None:
    """Retain real command exits without hiding failures or disabling TLS."""
    with log.open("ab") as stream:
        result = subprocess.run(
            args, cwd=cwd, stdout=stream, stderr=subprocess.STDOUT, timeout=300
        )
    if result.returncode:
        raise RuntimeError(
            f"{args[0]} failed with exit {result.returncode}; see {log.name}"
        )


def prepare(output: Path) -> None:
    """Build source-only product bytes and verify all 29 offline distributions."""
    if sys.platform != "win32" or platform.machine().lower() not in {"amd64", "x86_64"}:
        raise RuntimeError("The wheelhouse requires an actual Windows x64 host")
    output.mkdir(parents=True, exist_ok=False)
    wheels = output / "wheels"
    wheels.mkdir()
    record: dict[str, object] = {
        "source": SOURCE,
        "head": os.environ.get("GITHUB_SHA"),
        "python": sys.version,
        "platform": platform.platform(),
        "constraints_sha256": file_hash(CONSTRAINTS),
        "state": "preparing",
        "no_authentication_or_live_service": True,
    }
    manifest = output / "manifest.json"
    try:
        candidate = json.loads(
            (ROOT / ".github" / "rust-fast-candidate.json").read_text()
        )
        head = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
        ).strip()
        if (
            candidate != {"source": SOURCE, "lane": "windows-wheelhouse"}
            or head != record["head"]
            or not os.environ.get("GITHUB_REF", "").startswith(
                "refs/heads/validation/rust-fast-wheelhouse-"
            )
        ):
            raise ValueError(
                "Wrong immutable source, hosted head or wheelhouse namespace"
            )
        requested = pins(CONSTRAINTS.read_text())
        if len(requested) != 28:
            raise ValueError(
                "Expected 27 baseline dependencies plus one Windows broker runtime"
            )
        log = output / "preparation.log"
        with tempfile.TemporaryDirectory(prefix="rust-wheelhouse-") as directory:
            temporary = Path(directory)
            archive = temporary / "source.zip"
            run(
                ["git", "archive", "--format=zip", f"--output={archive}", SOURCE],
                ROOT,
                log,
            )
            source = temporary / "source"
            with zipfile.ZipFile(archive) as content:
                content.extractall(source)
            record["source_archive_sha256"] = file_hash(archive)
            run(
                [
                    sys.executable,
                    "-m",
                    "pip",
                    "install",
                    "--disable-pip-version-check",
                    "--index-url",
                    "https://pypi.org/simple",
                    "--no-deps",
                    BUILD_REQUIREMENT,
                ],
                temporary,
                log,
            )
            epoch = subprocess.check_output(
                ["git", "show", "-s", "--format=%ct", SOURCE], cwd=ROOT, text=True
            ).strip()
            os.environ["SOURCE_DATE_EPOCH"] = epoch
            run(
                [
                    sys.executable,
                    "-m",
                    "pip",
                    "wheel",
                    "--disable-pip-version-check",
                    "--no-deps",
                    "--no-build-isolation",
                    "--wheel-dir",
                    str(wheels),
                    str(source),
                ],
                temporary,
                log,
            )
            product = list(wheels.glob("ms_fabric_cli-*.whl"))
            if len(product) != 1 or wheel_identity(product[0]) != (
                "ms-fabric-cli",
                "1.8.0",
            ):
                raise ValueError("Unexpected product wheel identity")
            record["product_source_files_verified"] = verify_product_wheel(
                product[0], source
            )
            record["product_build_requirement"] = BUILD_REQUIREMENT
            record["source_date_epoch"] = epoch
            run(
                [
                    sys.executable,
                    "-m",
                    "pip",
                    "download",
                    "--disable-pip-version-check",
                    "--index-url",
                    "https://pypi.org/simple",
                    "--only-binary=:all:",
                    "--no-deps",
                    "--dest",
                    str(wheels),
                    "-r",
                    str(CONSTRAINTS),
                ],
                temporary,
                log,
            )
            observed: dict[str, str] = {}
            entries = []
            requirements = []
            for wheel in sorted(wheels.glob("*.whl")):
                name, version = wheel_identity(wheel)
                if name in observed:
                    raise ValueError(f"Duplicate wheel: {name}")
                observed[name] = version
                if name == "ms-fabric-cli":
                    entry = {
                        "filename": wheel.name,
                        "sha256": file_hash(wheel),
                        "bytes": wheel.stat().st_size,
                        "source": SOURCE,
                    }
                else:
                    with urllib.request.urlopen(
                        f"https://pypi.org/pypi/{name}/{version}/json", timeout=30
                    ) as response:
                        public = json.load(response)
                    entry = verify_public_wheel(wheel, public)
                entries.append({"name": name, "version": version, **entry})
                requirements.append(
                    f"{name}=={version} --hash=sha256:{entry['sha256']}"
                )
            if observed != {**requested, "ms-fabric-cli": "1.8.0"}:
                raise ValueError("Incomplete or unexpected wheelhouse distribution set")
            lock = output / "requirements-hashed.txt"
            lock.write_text("\n".join(requirements) + "\n", encoding="ascii")
            test_venv = temporary / "verify"
            run([sys.executable, "-m", "venv", str(test_venv)], temporary, log)
            interpreter = test_venv / "Scripts" / "python.exe"
            run(
                [
                    str(interpreter),
                    "-m",
                    "pip",
                    "install",
                    "--disable-pip-version-check",
                    "--no-index",
                    "--find-links",
                    str(wheels),
                    "--require-hashes",
                    "-r",
                    str(lock),
                ],
                temporary,
                log,
            )
            run([str(interpreter), "-m", "pip", "check"], temporary, log)
            record.update(
                state="verified",
                wheels=entries,
                distribution_count=len(entries),
                requirements_sha256=file_hash(lock),
                offline_install_and_dependency_check="passed",
                product_import_or_authentication=False,
            )
    except Exception as error:
        record.update(state="failed", error=f"{type(error).__name__}: {error}")
        raise
    finally:
        manifest.write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")


def main() -> None:
    """Run only the finite Windows wheelhouse preparation."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    prepare(parser.parse_args().output.resolve())


if __name__ == "__main__":
    main()
