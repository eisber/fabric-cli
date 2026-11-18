# Copyright (c) Microsoft Corporation. Licensed under the MIT License.
"""Build/verify one unsigned native preview bundle. Python 3.11+, standard library only."""

import argparse
from datetime import datetime, timezone
import hashlib
import io
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile

from check_boundaries import validate
from native_host import ROOT, TARGETS, command, evidence

REPOSITORY = ROOT.parent
REQUIRED_CONTENTS = {"LICENSE", "THIRD-PARTY-NOTICES.txt", "README.txt"}


def digest(data: bytes) -> str:
    """SHA-256 of exact bytes."""
    return hashlib.sha256(data).hexdigest()


def json_bytes(value: dict) -> bytes:
    """Encode release metadata deterministically."""
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def binary_name(target: str) -> str:
    """Return the sole executable name."""
    return "fab-rs.exe" if "windows" in target else "fab-rs"


def verify_binary(data: bytes, target: str) -> None:
    """Check executable format and architecture, not just its filename."""
    if "windows" in target:
        if data[:2] != b"MZ" or len(data) < 64:
            raise ValueError("Expected Windows PE")
        offset = struct.unpack_from("<I", data, 60)[0]
        if data[offset:offset + 4] != b"PE\0\0":
            raise ValueError("Invalid PE header")
        if struct.unpack_from("<H", data, offset + 4)[0] != 0x8664:
            raise ValueError("Expected x64 PE")
    elif "linux" in target:
        if data[:6] != b"\x7fELF\x02\x01" or struct.unpack_from("<H", data, 18)[0] != 62:
            raise ValueError("Expected little-endian x64 ELF")
    else:
        cpu = 0x0100000C if target.startswith("aarch64") else 0x01000007
        if data[:4] != b"\xcf\xfa\xed\xfe" or struct.unpack_from("<I", data, 4)[0] != cpu:
            raise ValueError("Expected native 64-bit Mach-O")


def license_files(package: dict, *, allow_missing: bool = False) -> list[Path]:
    """Collect shipped license/copyright/notice files, including nested native sources."""
    root = Path(package["manifest_path"]).resolve().parent
    files = set()
    declared = package.get("license_file")
    if declared:
        path = (root / declared).resolve()
        if not path.is_relative_to(root) or not path.is_file():
            raise ValueError(f"Unusable license_file for {package['name']}")
        files.add(path)
    for path in root.rglob("*"):
        legal_name = re.match(r"(?i)^(licen[cs]e|copying|copyright|notice)", path.name)
        legal_directory = any(
            re.match(r"(?i)^(licen[cs]es?|notices?)$", parent.name)
            for parent in path.relative_to(root).parents if parent != Path(".")
        )
        if path.is_file() and (legal_name or legal_directory):
            if not path.resolve().is_relative_to(root):
                raise ValueError(f"License symlink escapes package: {package['name']}")
            files.add(path.resolve())
    if not files and not allow_missing:
        raise ValueError(f"No shipped license text found for {package['name']}; resolve before packaging")
    return sorted(files)


def reviewed_license_inputs(package: dict, checksum: str) -> tuple[str | None, list[dict]]:
    """Read only explicit package/version exceptions; validate identity and bytes offline."""
    directory = (ROOT / "packaging").resolve()
    records = tomllib.loads((directory / "license-inputs.toml").read_text(encoding="utf-8"))["package"]
    matches = [
        entry for entry in records
        if (entry["name"], entry["version"]) == (package["name"], package["version"])
    ]
    if not matches:
        return None, []
    if len(matches) != 1:
        raise ValueError(f"Duplicate reviewed license input: {package['name']}")
    entry = matches[0]
    if (
        checksum != entry["checksum"]
        or package.get("license") != entry["license"]
        or (package.get("repository") or "").rstrip("/") != entry["repository"]
        or entry["selected_license"] not in entry["license"].split(" OR ")
        or not re.fullmatch(r"https://github\.com/[\w.-]+/[\w.-]+", entry["repository"])
        or not re.fullmatch(r"[a-f0-9]{40}", entry["revision"])
    ):
        raise ValueError(f"Reviewed license identity mismatch: {package['name']}")
    root = Path(package["manifest_path"]).resolve().parent
    vcs_path = root / ".cargo_vcs_info.json"
    if not vcs_path.is_file() or not vcs_path.resolve().is_relative_to(root):
        raise ValueError(f"Missing reviewed license VCS identity: {package['name']}")
    vcs = json.loads(vcs_path.read_text(encoding="utf-8"))
    if (
        vcs.get("git", {}).get("sha1") != entry["revision"]
        or vcs.get("path_in_vcs") != entry["path_in_vcs"]
    ):
        raise ValueError(f"Reviewed license VCS mismatch: {package['name']}")
    inputs = []
    for item in entry["files"]:
        base = root if item.get("shipped", False) else directory
        path = (base / item["file"]).resolve()
        if not path.is_relative_to(base) or not path.is_file():
            raise ValueError(f"Unusable reviewed license file: {package['name']}")
        data = path.read_bytes()
        if not data.strip() or digest(data) != item["sha256"]:
            raise ValueError(f"Reviewed license file checksum mismatch: {package['name']}")
        url = (
            entry["repository"].replace("https://github.com/", "https://raw.githubusercontent.com/")
            + f"/{entry['revision']}/{item['source_path']}"
        )
        inputs.append({"file": item["file"], "url": url, "sha256": item["sha256"], "text": data.decode("utf-8")})
    if not inputs:
        raise ValueError(f"Empty reviewed license input: {package['name']}")
    return entry["selected_license"], inputs


def legal_and_sbom(metadata: dict, lock: dict, source: str, version: str) -> tuple[bytes, dict]:
    """Include full resolved-graph notices and a CycloneDX inventory (a conservative superset)."""
    notices = [
        "THIRD-PARTY NOTICES — fab-rs\n"
        "Full Cargo resolved graph, including build/dev and target-specific packages.\n"
        "This conservative inventory is not a claim that every component is linked.\n"
    ]
    components = []
    # Cargo IDs for path packages contain absolute checkout paths. Never publish them.
    references = {
        p["id"]: f"pkg:cargo/{p['name']}@{p['version']}"
        for p in metadata["packages"]
    }
    if len(set(references.values())) != len(references):
        raise ValueError("Ambiguous same-name/version package sources; resolve before packaging")
    lock_packages = {(p["name"], p["version"], p.get("source")): p for p in lock["package"]}
    for package in sorted(metadata["packages"], key=lambda p: p["id"]):
        if package["source"] is not None and package["source"] not in (
            "registry+https://github.com/rust-lang/crates.io-index",
            "sparse+https://index.crates.io/",
        ):
            raise ValueError("Unapproved non-public dependency source; do not expose it in metadata")
        license_id = package.get("license")
        if not license_id and not package.get("license_file"):
            raise ValueError(f"Missing declared license: {package['name']}")
        component = {
            "type": "application" if package["name"] == "fabric-cli" else "library",
            "bom-ref": references[package["id"]], "name": package["name"],
            "version": package["version"],
            "properties": [
                {"name": "cargo:source", "value": package.get("source") or f"git:{source}"},
                {"name": "cargo:license", "value": license_id or "license-file"},
            ],
        }
        locked = lock_packages.get((package["name"], package["version"], package.get("source")), {})
        if package["source"] is not None and not locked.get("checksum"):
            raise ValueError(f"Missing registry checksum in lockfile: {package['name']}")
        if locked.get("checksum"):
            component["hashes"] = [{"alg": "SHA-256", "content": locked["checksum"]}]
        if package["source"] is not None:
            notices.append(f"\n===== {package['name']} {package['version']} ({license_id or 'license-file'}) =====\n")
            package_root = Path(package["manifest_path"]).resolve().parent
            # Missing discovery alone can use an explicit input; malformed declared paths
            # or escaping symlinks remain fatal, including for reviewed packages.
            shipped = license_files(package, allow_missing=True)
            choice, inputs = reviewed_license_inputs(package, locked["checksum"])
            if not shipped and not inputs:
                raise ValueError(
                    f"No shipped license text found for {package['name']}; "
                    "add a reviewed package-specific input before packaging"
                )
            for path in shipped:
                notices.append(f"\n--- {path.relative_to(package_root).as_posix()} ---\n")
                text = path.read_text(encoding="utf-8")
                if not text.strip():
                    raise ValueError(f"Empty shipped license text: {package['name']}")
                notices.append(text)
                notices.append("\n")
            if inputs:
                component["properties"].append({"name": "cargo:reviewed-license-choice", "value": choice})
                component["externalReferences"] = [
                    {"type": "license", "url": item["url"],
                     "hashes": [{"alg": "SHA-256", "content": item["sha256"]}]}
                    for item in inputs
                ]
                notices.append(f"\nReviewed package-specific license choice: {choice}\n")
                for item in inputs:
                    notices.append(
                        f"\n--- Reviewed input: {item['file']} ---\n"
                        f"Authoritative source: {item['url']}\nSHA-256: {item['sha256']}\n\n"
                    )
                    notices.append(item["text"])
                    notices.append("\n")
        components.append(component)
    resolve = metadata.get("resolve")
    if not resolve:
        raise ValueError("Resolved dependency graph required for SBOM")
    sbom = {
        "bomFormat": "CycloneDX", "specVersion": "1.5", "version": 1,
        "metadata": {
            "component": {"type": "application", "name": "fab-rs", "version": version},
            "properties": [{"name": "source:revision", "value": source}],
        },
        "components": components,
        "dependencies": [
            {"ref": references[node["id"]], "dependsOn": sorted({references[d["pkg"]] for d in node["deps"]})}
            for node in resolve["nodes"]
        ],
    }
    return "".join(notices).encode("utf-8"), sbom


def archive_bytes(contents: dict[str, bytes], target: str) -> bytes:
    """Create a flat, minimal ZIP/tar.gz with only the allowed product files."""
    buffer = io.BytesIO()
    if "windows" in target:
        with zipfile.ZipFile(buffer, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for name, data in sorted(contents.items()):
                archive.writestr(name, data)
    else:
        with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
            for name, data in sorted(contents.items()):
                info = tarfile.TarInfo(name)
                info.size = len(data)
                info.mode = 0o755 if name == binary_name(target) else 0o644
                archive.addfile(info, io.BytesIO(data))
    return buffer.getvalue()


def unpack(data: bytes, target: str) -> dict[str, bytes]:
    """Read allowed regular files only; never extract arbitrary archive paths or links."""
    files = {}
    allowed = REQUIRED_CONTENTS | {binary_name(target)}
    if "windows" in target:
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            for info in archive.infolist():
                if info.filename not in allowed or info.filename in files or info.is_dir():
                    raise ValueError("Unexpected/duplicate ZIP entry")
                if (info.external_attr >> 16) & 0o170000 == 0o120000:
                    raise ValueError("ZIP symlink forbidden")
                files[info.filename] = archive.read(info)
    else:
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
            for info in archive:
                if info.name not in allowed or info.name in files or not info.isfile():
                    raise ValueError("Unexpected/duplicate/non-regular tar entry")
                stream = archive.extractfile(info)
                if stream is None:
                    raise ValueError("Unreadable tar member")
                files[info.name] = stream.read()
                if info.name == binary_name(target) and info.mode != 0o755:
                    raise ValueError("Executable mode missing from tar")
    if set(files) != allowed or any(not content for content in files.values()):
        raise ValueError("Missing or empty bundle content")
    verify_binary(files[binary_name(target)], target)
    return files


def smoke(binary: Path, version: str) -> None:
    """Run exact extracted executable with no Azure CLI on PATH or real user config."""
    with tempfile.TemporaryDirectory(prefix="fab-rs-smoke-") as home:
        env = os.environ.copy()
        for key in list(env):
            if key.upper().startswith(("AZURE_", "FAB_", "FABRIC_")):
                del env[key]
        env.update({
            "PATH": "", "HOME": home, "USERPROFILE": home, "APPDATA": home,
            "LOCALAPPDATA": home, "XDG_CONFIG_HOME": home, "AZURE_CONFIG_DIR": home,
            "XDG_DATA_HOME": home, "XDG_CACHE_HOME": home, "XDG_STATE_HOME": home,
        })
        def run(*args: str) -> subprocess.CompletedProcess:
            return subprocess.run(
                [str(binary.resolve()), *args], env=env, cwd=home, capture_output=True,
                text=True, timeout=30, check=False,
            )
        for args in (("--help",), ("--version",), ("api", "--help"), ("auth", "--help"),
                     ("auth", "login", "--help"), ("auth", "status", "--help"),
                     ("auth", "logout", "--help")):
            result = run(*args)
            if result.returncode or not result.stdout.strip() or result.stderr.strip():
                raise ValueError(f"Offline metadata smoke failed: {args}")
            if args == ("--version",) and not re.fullmatch(
                rf"fab-rs {re.escape(version)}\s*", result.stdout
            ):
                raise ValueError("Executable version does not match package version")
        # No Rust binding: must fail before credential acquisition/HTTP.
        for args in (("auth", "status"), ("api", "workspaces")):
            result = run(*args)
            if result.returncode == 0:
                raise ValueError(f"Unbound offline command unexpectedly succeeded: {args}")
            envelope = json.loads(result.stdout)
            if envelope.get("status") != "Failure":
                raise ValueError("Expected structured command failure")


def build(target: str, output: Path) -> None:
    """Assemble release-level metadata and one minimal unsigned preview archive."""
    observed = evidence(target)
    source = command("git", "-C", str(REPOSITORY), "rev-parse", "HEAD")
    if not re.fullmatch("[0-9a-f]{40}", source):
        raise ValueError("Expected immutable full source revision")
    dirty = command("git", "-C", str(REPOSITORY), "status", "--porcelain", "--untracked-files=normal")
    if dirty:
        raise ValueError("Refusing provenance for a dirty source checkout")
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--offline", "--locked", "--format-version", "1",
         "--filter-platform", target], cwd=ROOT, text=True, timeout=120,
    ))
    validate(metadata, ROOT)
    app = [p for p in metadata["packages"] if p["name"] == "fabric-cli" and p["id"] in metadata["workspace_members"]]
    if len(app) != 1:
        raise ValueError("Expected exactly one fabric-cli workspace application")
    version = app[0]["version"]
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version):
        raise ValueError("Unsupported release version")
    target_dir = Path(metadata["target_directory"])
    binary = target_dir / target / "release" / binary_name(target)
    data = binary.read_bytes()
    verify_binary(data, target)
    lock_bytes = (ROOT / "Cargo.lock").read_bytes()
    notices, sbom = legal_and_sbom(metadata, tomllib.loads(lock_bytes.decode()), source, version)
    contents = {
        binary_name(target): data,
        "LICENSE": (REPOSITORY / "LICENSE").read_bytes(),
        "THIRD-PARTY-NOTICES.txt": notices,
        "README.txt": (ROOT / "packaging" / "README.txt").read_bytes(),
    }
    stem = f"fab-rs-{version}-{target}"
    archive_name = stem + (".zip" if "windows" in target else ".tar.gz")
    provenance = {
        "product": "fab-rs", "version": version, "source_revision": source,
        "source_repository": "https://github.com/eisber/fabric-cli",
        "target": target, "toolchain": observed,
        "build_time_utc": datetime.now(timezone.utc).isoformat(),
        "cargo_lock_sha256": digest(lock_bytes), "unsigned_unofficial_preview": True,
        "build_command": f"cargo build --offline --locked --release -p fabric-cli --bin fab-rs --target {target}",
        "archive": archive_name,
        "contents_sha256": {name: digest(value) for name, value in contents.items()},
        "compatibility": binary_compatibility(binary, target),
    }
    assets = {
        archive_name: archive_bytes(contents, target),
        stem + ".cdx.json": json_bytes(sbom),
        stem + ".provenance.json": json_bytes(provenance),
    }
    if output.exists():
        raise ValueError("Output directory already exists; refusing stale/mixed assets")
    output.mkdir(parents=True)
    for name, value in assets.items():
        (output / name).write_bytes(value)
    (output / "SHA256SUMS").write_text(
        "".join(f"{digest(value)}  {name}\n" for name, value in sorted(assets.items())),
        encoding="utf-8",
    )
    print(f"Packaged UNSIGNED UNOFFICIAL {archive_name}")


def binary_compatibility(binary: Path, target: str) -> dict:
    """Record native loader evidence without guessing a supported minimum OS."""
    def tool(*args: str) -> str:
        # otool echoes the supplied pathname. Use only a basename to avoid publishing
        # the user's checkout path or native runner workspace identifiers.
        return subprocess.check_output(
            [*args, binary.name], cwd=binary.parent, text=True, timeout=60,
        ).strip()
    if "linux" in target:
        return {
            "elf_version_info": tool("readelf", "--version-info"),
            "elf_dynamic": tool("readelf", "--dynamic"),
            "minimum_tested_os": None,
        }
    if "apple" in target:
        return {
            "mach_o_load_commands": tool("otool", "-l"),
            "mach_o_libraries": tool("otool", "-L"),
            "minimum_tested_os": None,
        }
    return {"minimum_tested_os": None}


def verify(target: str, directory: Path, run_smoke: bool) -> None:
    """Verify the complete expected asset set, hashes, archive contents and provenance."""
    manifests = list(directory.glob("*.provenance.json"))
    if len(manifests) != 1:
        raise ValueError("Expected one provenance record")
    provenance = json.loads(manifests[0].read_text())
    version = provenance["version"]
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version):
        raise ValueError("Invalid provenance version")
    stem = f"fab-rs-{version}-{target}"
    archive = stem + (".zip" if "windows" in target else ".tar.gz")
    expected = {archive, stem + ".cdx.json", stem + ".provenance.json"}
    if ({p.name for p in directory.iterdir()} != expected | {"SHA256SUMS"}
        or any(p.is_symlink() or not p.is_file() for p in directory.iterdir())):
        raise ValueError("Missing or unexpected release assets")
    checksums = {}
    for line in (directory / "SHA256SUMS").read_text().splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]+)", line)
        if not match or match[2] in checksums:
            raise ValueError("Malformed/duplicate checksum entry")
        checksums[match[2]] = match[1]
    if set(checksums) != expected:
        raise ValueError("Checksum inventory mismatch")
    for name, checksum in checksums.items():
        if digest((directory / name).read_bytes()) != checksum:
            raise ValueError(f"Checksum mismatch: {name}")
    expected_source = command("git", "-C", str(REPOSITORY), "rev-parse", "HEAD")
    pinned = tomllib.loads((ROOT / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    expected_version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    if (provenance["target"] != target or provenance["archive"] != archive
        or provenance["product"] != "fab-rs"
        or version != expected_version
        or provenance["unsigned_unofficial_preview"] is not True
        or not re.fullmatch("[0-9a-f]{40}", provenance["source_revision"])
        or provenance["source_revision"] != expected_source
        or provenance["toolchain"]["target"] != target
        or provenance["toolchain"]["toolchain"] != pinned):
        raise ValueError("Provenance identity mismatch")
    sbom = json.loads((directory / (stem + ".cdx.json")).read_text())
    if (sbom.get("bomFormat") != "CycloneDX" or not sbom.get("components")
        or not sbom.get("dependencies")
        or sbom["metadata"]["component"]["version"] != version
        or {"name": "source:revision", "value": provenance["source_revision"]}
        not in sbom["metadata"]["properties"]):
        raise ValueError("Missing/inconsistent resolved SBOM")
    contents = unpack((directory / archive).read_bytes(), target)
    if {name: digest(value) for name, value in contents.items()} != provenance["contents_sha256"]:
        raise ValueError("Archived content differs from provenance")
    if contents["LICENSE"] != (REPOSITORY / "LICENSE").read_bytes():
        raise ValueError("Product MIT license differs from source")
    if run_smoke:
        evidence(target)
        with tempfile.TemporaryDirectory(prefix="fab-rs-bundle-") as temporary:
            binary = Path(temporary) / binary_name(target)
            binary.write_bytes(contents[binary.name])
            binary.chmod(0o755)
            smoke(binary, version)
    print(f"Verified exact unsigned preview asset set: {target}; smoke={run_smoke}")


def main() -> None:
    """Package or verify one target; never sign, upload, publish, or invoke Cargo builds."""
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    create = sub.add_parser("build")
    create.add_argument("--target", choices=TARGETS, required=True)
    create.add_argument("--output", type=Path, required=True)
    check = sub.add_parser("verify")
    check.add_argument("--target", choices=TARGETS, required=True)
    check.add_argument("--directory", type=Path, required=True)
    check.add_argument("--smoke", action="store_true")
    args = parser.parse_args()
    if args.action == "build":
        build(args.target, args.output)
    else:
        verify(args.target, args.directory, args.smoke)


if __name__ == "__main__":
    main()
