# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Offline two-binary assembly, separate from the schema-1 preview contract.

Inputs and the verifier are independently held controller pins, never discovered
from an asset. No build, download, signing, installation or publishing occurs.
Only explicitly approved, digest-pinned OpenSSL verification may execute.
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
import re
import stat
import struct
import subprocess
import sys
import time
from pathlib import Path, PurePosixPath, PureWindowsPath
from typing import Any

METADATA_CAP = 1024 * 1024
BINARY_CAP = 64 * 1024 * 1024
MANIFEST_CAP = 16 * 1024
VALIDITY_CAP = 7 * 24 * 60 * 60
REPOSITORY = "microsoft/fabric-cli"
ASSEMBLY_NAME = "native-assembly-v1.json"
BUILD_NAME = "native-build-v1.json"
DIRTY_NAME = "dirty-source-v1.json"
TARGETS = {
    "x86_64-pc-windows-msvc": ("windows", "x86_64"),
    "aarch64-pc-windows-msvc": ("windows", "aarch64"),
    "x86_64-unknown-linux-gnu": ("linux", "x86_64"),
    "aarch64-unknown-linux-gnu": ("linux", "aarch64"),
    "x86_64-apple-darwin": ("macos", "x86_64"),
    "aarch64-apple-darwin": ("macos", "aarch64"),
}
FEATURE_ORDER = (
    "certificate-store-windows", "headless", "native-host", "native-read", "repl", "windows-broker",
    "interactive", "certificate-store-pkcs11", "certificate-store-macos",
)
ROOT_FEATURES = set(FEATURE_ORDER) | {"self-update"}
EXPECT_FIELDS = {
    "schema", "product", "repository", "source_commit", "dirty_manifest_sha256",
    "target", "platform", "version", "tag", "channel", "variant",
    "explicit_features", "public_key_hex", "build_record_sha256",
    "artifacts", "installation", "issued_at", "expires_at",
    "signing_policy_sha256", "os_attestation_sha256",
}
POLICY_FIELDS = {
    "schema", "public_key_hex", "public_key_policy_ref",
    "native_signer_policy_ref", "native_verification_policy_ref",
    "windows_signing_profile_ref", "apple_developer_id_ref",
    "apple_notarization_policy_ref", "linux_signer_ref",
    "production_workflow_ref", "release_authorization_ref", "verifier",
}


class AssemblyError(ValueError):
    """A fixed diagnostic, never rejected input text or process output."""


def require(condition: bool, message: str) -> None:
    """Reject an invalid contract using a stable diagnostic."""
    if not condition:
        raise AssemblyError(message)


def sha(data: bytes) -> str:
    """Hash exact bytes, without JSON or line-ending normalization."""
    return hashlib.sha256(data).hexdigest()


def matches(pattern: str, value: Any) -> bool:
    return isinstance(value, str) and re.fullmatch(pattern, value) is not None


def digest(value: Any, nullable: bool = False) -> None:
    require(
        (nullable and value is None) or matches(r"[a-f0-9]{64}", value),
        "Invalid digest pin",
    )


def keys(value: Any, fields: set[str]) -> dict[str, Any]:
    require(type(value) is dict and set(value) == fields, "Unknown or missing fields")
    return value


def schema(value: dict[str, Any], revision: int = 1) -> None:
    require(type(value["schema"]) is int and value["schema"] == revision,
            "Unsupported schema")


def text(value: Any) -> None:
    require(isinstance(value, str) and 0 < len(value) <= 512
            and all(32 <= ord(c) < 127 for c in value), "Invalid public reference")


def pairs(values: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in values:
        require(key not in result, "Duplicate JSON field")
        result[key] = value
    return result


def nonfinite(_: str) -> Any:
    raise AssemblyError("Nonfinite JSON number")


def floating_point(_: str) -> Any:
    raise AssemblyError("Floating-point JSON number forbidden")


def parse(raw: bytes) -> Any:
    try:
        return json.loads(raw.decode("utf-8"), object_pairs_hook=pairs,
                          parse_constant=nonfinite, parse_float=floating_point)
    except (UnicodeError, ValueError, RecursionError) as error:
        raise AssemblyError("Invalid bounded UTF-8 JSON") from error


def encode(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, indent=2, allow_nan=False)
            + "\n").encode("utf-8")


def plain(path: Path, directory: bool = False) -> os.stat_result:
    info = path.lstat()
    require(not path.is_symlink() and not getattr(info, "st_file_attributes", 0)
            & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400),
            "Links or reparse points are forbidden")
    require(stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode),
            "Expected plain directory or regular file")
    if not directory:
        require(info.st_nlink == 1, "Hard links are forbidden")
    return info


def absolute(path: Path, existing: bool = True) -> Path:
    require(path.is_absolute() and ".." not in path.parts,
            "Absolute non-traversing path required")
    for parent in reversed(path.parents):
        plain(parent, True)
    if existing:
        plain(path, path.is_dir())
    return path


def read(path: Path, cap: int = METADATA_CAP) -> bytes:
    absolute(path)
    before = plain(path)
    require(0 < before.st_size <= cap, "Input exceeds size bounds")
    with path.open("rb") as stream:
        opened = os.fstat(stream.fileno())
        require((opened.st_dev, opened.st_ino) == (before.st_dev, before.st_ino),
                "Input identity changed")
        data = stream.read(cap + 1)
        after = os.fstat(stream.fileno())
    require(len(data) == before.st_size and len(data) <= cap
            and (after.st_size, after.st_mtime_ns) ==
            (before.st_size, before.st_mtime_ns), "Input changed while reading")
    final = plain(path)
    require((final.st_dev, final.st_ino, final.st_size, final.st_mtime_ns) ==
            (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns),
            "Input identity changed")
    return data


def leaf(value: Any) -> None:
    require(matches(r"[A-Za-z0-9][A-Za-z0-9_.+-]{0,159}", value)
            and ".." not in value and not value.endswith((".", " ")),
            "Invalid inventory leaf")
    require(value.split(".")[0].upper() not in {
        "CON", "PRN", "AUX", "NUL",
        *(f"COM{i}" for i in range(1, 10)), *(f"LPT{i}" for i in range(1, 10)),
    }, "Reserved inventory leaf")


def version(value: Any) -> None:
    require(isinstance(value, str) and len(value) <= 100, "Invalid version length")
    number = r"(?:0|[1-9][0-9]*)"
    require(matches(rf"{number}\.{number}\.{number}"
                    r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
                    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?", value),
            "Invalid canonical semantic version")
    core, _, pre = value.partition("+")[0].partition("-")
    require(all(int(n) <= 2**64 - 1 for n in core.split(".")),
            "Version component exceeds protocol bound")
    require(all(not (part.isdigit() and len(part) > 1 and part.startswith("0"))
                for part in pre.split(".")), "Noncanonical prerelease")
    leaf("rust-v" + value)


def feature_identity(explicit: Any) -> tuple[list[str], str]:
    require(type(explicit) is list and all(isinstance(v, str) for v in explicit),
            "Invalid feature list")
    require(explicit == sorted(set(explicit)) and set(explicit) <= ROOT_FEATURES,
            "Unsupported or duplicate feature")
    resolved = set(explicit)
    if "native-read" in resolved:
        resolved.update(("native-host", "windows-broker"))
    if {"certificate-store-windows", "certificate-store-pkcs11", "certificate-store-macos"} & resolved:
        resolved.add("headless")
    if "interactive" in resolved:
        resolved.add("native-host")
    if "headless" in resolved:
        resolved.add("native-host")
    identity_features = resolved.copy()
    if "certificate-store-windows" in resolved:
        identity_features.difference_update(("headless", "native-host"))
        if "native-read" in resolved:
            identity_features.discard("windows-broker")
    identity = (["self-update"] if "self-update" in resolved else []) + [
        name for name in FEATURE_ORDER if name in identity_features
    ]
    if {"interactive", "certificate-store-pkcs11", "certificate-store-macos"} & resolved:
        payload = ("fabric-cli-feature-v2\0" + "\0".join(sorted(resolved))).encode("utf-8")
        result = "f2-" + hashlib.sha256(payload).hexdigest()[:60]
    else:
        result = "+".join(identity) or "default"
    require(len(result) <= 64, "Variant exceeds updater identity bound")
    return sorted(resolved), result


def pin(value: Any, role: str) -> dict[str, Any]:
    result = keys(value, {"role", "filename", "sha256", "bytes"})
    require(result["role"] == role, "Incorrect artifact role")
    leaf(result["filename"])
    digest(result["sha256"])
    require(type(result["bytes"]) is int and 0 < result["bytes"] <= BINARY_CAP,
            "Invalid artifact length")
    return result


def names(expected: dict[str, Any]) -> tuple[str, str, str]:
    suffix = ".exe" if expected["platform"]["os"] == "windows" else ""
    return (
        f"fab-rs-{expected['target']}-{expected['variant']}{suffix}",
        f"fab-rs-update-helper{suffix}",
        f"update-{expected['target']}-{expected['variant']}.json",
    )


def installation_path(value: Any, windows: bool) -> Any:
    require(isinstance(value, str) and len(value) <= 512 and "\0" not in value,
            "Invalid planned installation path")
    path = PureWindowsPath(value) if windows else PurePosixPath(value)
    require(path.is_absolute() and ".." not in path.parts,
            "Installation path must be absolute")
    if windows:
        require(matches(r"[A-Za-z]:", path.drive)
                and all(":" not in p and not p.endswith((".", " "))
                        for p in path.parts[1:]),
                "Only local DOS-drive installation paths are supported")
    return path


def load_expected(path: Path, expected_sha256: str) -> tuple[dict[str, Any], bytes]:
    """Load independently held controller pins, not metadata from the bundle."""
    digest(expected_sha256)
    raw = read(path)
    require(sha(raw) == expected_sha256, "Expectation digest mismatch")
    e = keys(parse(raw), EXPECT_FIELDS)
    schema(e)
    require(e["product"] == "fab-rs" and e["repository"] == REPOSITORY,
            "Incorrect official product origin")
    require(matches(r"[a-f0-9]{40}", e["source_commit"]), "Full source SHA required")
    for field in ("dirty_manifest_sha256", "signing_policy_sha256",
                  "os_attestation_sha256", "public_key_hex"):
        digest(e[field], True)
    require(e["public_key_hex"] != "0" * 64, "Zero publisher key forbidden")
    digest(e["build_record_sha256"])
    require(isinstance(e["target"], str) and e["target"] in TARGETS,
            "Unsupported target")
    p = keys(e["platform"], {
        "os", "architecture", "minimum_os", "runtime_floor", "evidence_ref"
    })
    require((p["os"], p["architecture"]) == TARGETS[e["target"]],
            "Target OS or architecture mismatch")
    for field in ("minimum_os", "runtime_floor", "evidence_ref"):
        text(p[field])
    version(e["version"])
    require(e["tag"] == "rust-v" + e["version"], "Incorrect Rust release tag")
    require(e["channel"] in ("stable", "preview"), "Missing release channel")
    require((e["channel"] == "preview") == ("-" in e["version"].split("+")[0]),
            "Version/channel mismatch")
    resolved, variant = feature_identity(e["explicit_features"])
    require(e["variant"] == variant, "Compiled feature variant mismatch")
    require(p["os"] == "windows" or not
            ({"native-read", "windows-broker"} & set(resolved)),
            "Windows-only features on another OS")
    a = keys(e["artifacts"], {"cli", "helper"})
    cli, helper, metadata = names(e)
    for role, filename in (("cli", cli), ("helper", helper)):
        require(pin(a[role], role)["filename"] == filename,
                "Incorrect artifact inventory name")
    leaf(metadata)
    i = keys(e["installation"], {"custody", "root", "windows_system_root"})
    require(i["custody"] in ("standalone", "msi", "winget", "homebrew", "deb", "rpm"),
            "Unknown installation custody")
    allowed = {
        "windows": {"standalone", "msi", "winget"},
        "macos": {"standalone", "homebrew"},
        "linux": {"standalone", "deb", "rpm"},
    }
    require(i["custody"] in allowed[p["os"]], "Manager/OS mismatch")
    installation_path(i["root"], p["os"] == "windows")
    if p["os"] == "windows":
        installation_path(i["windows_system_root"], True)
    else:
        require(i["windows_system_root"] is None, "Non-Windows SystemRoot forbidden")
    require(type(e["issued_at"]) is int and type(e["expires_at"]) is int
            and 0 < e["issued_at"] < e["expires_at"] <= 2**64 - 1
            and e["expires_at"] - e["issued_at"] <= VALIDITY_CAP,
            "Invalid metadata validity window")
    return e, raw


def validate_binary(data: bytes, target: str) -> None:
    """Check only the protocol's native executable header, never execute bytes."""
    operating_system, architecture = TARGETS[target]
    valid = False
    if operating_system == "windows" and len(data) >= 64 and data[:2] == b"MZ":
        offset = struct.unpack_from("<I", data, 0x3c)[0]
        if 64 <= offset <= len(data) - 26:
            machine = struct.unpack_from("<H", data, offset + 4)[0]
            flags = struct.unpack_from("<H", data, offset + 22)[0]
            magic = struct.unpack_from("<H", data, offset + 24)[0]
            valid = (data[offset:offset + 4] == b"PE\0\0"
                     and machine == (0x8664 if architecture == "x86_64" else 0xaa64)
                     and flags & 2 != 0 and flags & 0x2000 == 0 and magic == 0x20b)
    elif operating_system == "linux" and len(data) >= 64:
        kind, machine = struct.unpack_from("<HH", data, 16)
        valid = (data.startswith(b"\x7fELF\x02\x01\x01") and kind in (2, 3)
                 and machine == (62 if architecture == "x86_64" else 183))
    elif operating_system == "macos" and len(data) >= 32:
        machine = struct.unpack_from("<I", data, 4)[0]
        valid = (data[:4] == b"\xcf\xfa\xed\xfe"
                 and machine == (0x01000007 if architecture == "x86_64" else 0x0100000c)
                 and struct.unpack_from("<I", data, 12)[0] == 2)
    require(valid, "Native executable header mismatch")


def validate_build(raw: bytes, e: dict[str, Any]) -> None:
    require(sha(raw) == e["build_record_sha256"], "Build record digest mismatch")
    b = keys(parse(raw), {
        "schema", "source_commit", "dirty_manifest_sha256", "toolchain",
        "rustc_sha256", "cargo_sha256", "lockfile_sha256", "build_command_ref",
        "cli", "helper",
    })
    schema(b)
    require(b["source_commit"] == e["source_commit"] and
            b["dirty_manifest_sha256"] == e["dirty_manifest_sha256"],
            "Build/source binding mismatch")
    require(b["toolchain"] == "1.99.1", "Unreviewed Rust toolchain")
    for field in ("rustc_sha256", "cargo_sha256", "lockfile_sha256"):
        digest(b[field])
    text(b["build_command_ref"])
    resolved, _ = feature_identity(e["explicit_features"])
    for role, package, binary in (
        ("cli", "fabric-cli", "fab-rs"),
        ("helper", "fabric-cli-update", "fab-rs-update-helper"),
    ):
        item = keys(b[role], {
            "package", "bin", "target", "host_target", "profile",
            "default_features", "explicit_features", "resolved_features",
            "compiled_pins", "artifact",
        })
        require(item["package"] == package and item["bin"] == binary
                and item["target"] == e["target"] == item["host_target"]
                and item["profile"] == "release" and item["default_features"] is False,
                "Non-native or incorrect build identity")
        require(item["explicit_features"] ==
                (e["explicit_features"] if role == "cli" else ["bounded-helper"])
                and item["resolved_features"] ==
                (resolved if role == "cli" else ["bounded-helper"]),
                "Unrecorded feature closure or helper binary build")
        compiled = keys(item["compiled_pins"], {
            "FAB_RELEASE_PUBLIC_KEY", "FAB_RELEASE_CHANNEL",
            "FAB_RELEASE_VARIANT", "FAB_RELEASE_INSTALL_CUSTODY",
        })
        custody = ("standalone" if e["installation"]["custody"] == "standalone"
                   else "package-manager")
        require(compiled == {
            "FAB_RELEASE_PUBLIC_KEY": e["public_key_hex"],
            "FAB_RELEASE_CHANNEL": e["channel"],
            "FAB_RELEASE_VARIANT": e["variant"],
            "FAB_RELEASE_INSTALL_CUSTODY": custody,
        }, "CLI/helper compiled publisher pins differ")
        pin(item["artifact"], role)
        require(item["artifact"] == e["artifacts"][role], "Build/artifact pin mismatch")


def validate_dirty(raw: bytes | None, e: dict[str, Any]) -> None:
    if e["dirty_manifest_sha256"] is None:
        require(raw is None, "Unexpected dirty manifest")
        return
    require(raw is not None and sha(raw) == e["dirty_manifest_sha256"],
            "Complete engineering dirty manifest required")
    record = keys(parse(raw), {"schema", "base_commit", "entries"})
    schema(record)
    require(record["base_commit"] == e["source_commit"], "Dirty base mismatch")
    entries = record["entries"]
    require(type(entries) is list and 0 < len(entries) <= 4096,
            "Invalid dirty inventory")
    seen: list[str] = []
    for entry in entries:
        item = keys(entry, {"path", "status", "sha256", "bytes", "mode"})
        path = item["path"]
        require(isinstance(path, str) and 0 < len(path) <= 512
                and "\\" not in path and ":" not in path
                and not path.startswith("/") and not path.endswith("/")
                and all(p not in ("", ".", "..") for p in path.split("/")),
                "Invalid dirty source path")
        require(path.casefold() not in {p.casefold() for p in seen},
                "Duplicate dirty path")
        seen.append(path)
        require(item["status"] in ("added", "modified", "deleted"), "Invalid dirty status")
        if item["status"] == "deleted":
            require(item["sha256"] is None and item["bytes"] is None
                    and item["mode"] is None, "Invalid deletion record")
        else:
            digest(item["sha256"])
            require(type(item["bytes"]) is int and 0 <= item["bytes"] <= BINARY_CAP
                    and item["mode"] in ("100644", "100755"), "Invalid dirty file pin")
    require(seen == sorted(seen), "Dirty manifest must be sorted")


def receipt(e: dict[str, Any]) -> tuple[str, bytes]:
    windows = e["platform"]["os"] == "windows"
    root = installation_path(e["installation"]["root"], windows)
    cli = str(root / ("fab-rs.exe" if windows else "fab-rs"))
    helper = str(root / names(e)[1])
    common = {
        "product": "fab-rs", "repository": REPOSITORY,
        "custody": e["installation"]["custody"], "executable": cli,
        "version": e["version"], "target": e["target"], "variant": e["variant"],
        "sha256": e["artifacts"]["cli"]["sha256"],
        "helper": helper, "helper_sha256": e["artifacts"]["helper"]["sha256"],
    }
    if e["installation"]["custody"] == "standalone":
        return ".fab-rs-install.json", encode({
            "schema": 2, **common, "backups": [],
            "windows_system_root": e["installation"]["windows_system_root"],
        })
    return "manager-custody-v1.json", encode({
        "schema": 1, **common, "auto_update": False,
    })


def payload(e: dict[str, Any], expected_raw: bytes, build_raw: bytes,
            cli: bytes, helper: bytes, dirty_raw: bytes | None) -> dict[str, bytes]:
    validate_build(build_raw, e)
    validate_dirty(dirty_raw, e)
    result: dict[str, bytes] = {BUILD_NAME: build_raw}
    for role, data in (("cli", cli), ("helper", helper)):
        p = e["artifacts"][role]
        require(len(data) == p["bytes"] and sha(data) == p["sha256"],
                "Artifact digest or size drift")
        validate_binary(data, e["target"])
        result[p["filename"]] = data
    receipt_name, receipt_raw = receipt(e)
    result[receipt_name] = receipt_raw
    if dirty_raw is not None:
        result[DIRTY_NAME] = dirty_raw
    update = {
        "protocol": 1, "product": "fab-rs", "repository": REPOSITORY,
        "tag": e["tag"], "version": e["version"], "channel": e["channel"],
        "target": e["target"], "variant": e["variant"],
        "asset": e["artifacts"]["cli"]["filename"],
        "bytes": len(cli), "sha256": sha(cli), "expires_at": e["expires_at"],
    }
    result[names(e)[2]] = encode(update)
    require(len(result[names(e)[2]]) <= MANIFEST_CAP, "Update metadata too large")
    inventory = [
        {"filename": name, "sha256": sha(data), "bytes": len(data),
         "role": ("cli" if name == names(e)[0] else
                  "helper" if name == names(e)[1] else "assembly-evidence")}
        for name, data in sorted(result.items())
    ]
    result[ASSEMBLY_NAME] = encode({
        "schema": 1, "kind": "native-two-binary-assembly",
        "product": "fab-rs", "repository": REPOSITORY, "tag": e["tag"],
        "version": e["version"], "channel": e["channel"], "target": e["target"],
        "variant": e["variant"], "source_commit": e["source_commit"],
        "dirty_manifest_sha256": e["dirty_manifest_sha256"],
        "platform": e["platform"], "expectation_sha256": sha(expected_raw),
        "signing_policy_sha256": e["signing_policy_sha256"],
        "os_attestation_sha256": e["os_attestation_sha256"],
        "installation": e["installation"],
        "update_policy": {"automatic": "same-installed-major",
                          "major_upgrade": "explicit-opt-in"},
        "inventory": inventory,
    })
    require(len({name.casefold() for name in result}) == len(result),
            "Duplicate inventory names")
    return result


def check_time(e: dict[str, Any], now: int) -> None:
    require(type(now) is int and e["issued_at"] <= now < e["expires_at"]
            and e["expires_at"] - now <= VALIDITY_CAP, "Expired or future metadata")


def output_bundle(output: Path, files: dict[str, bytes]) -> Path:
    """Reserve a new root, stage exclusively, then atomically expose its bundle."""
    absolute(output, False)
    output.mkdir(mode=0o700)
    stage = output / ".staging"
    written: dict[Path, tuple[int, int]] = {}
    try:
        stage.mkdir(mode=0o700)
        for name, data in sorted(files.items()):
            if name != ".fab-rs-install.json":
                leaf(name)
            path = stage / name
            with path.open("xb") as stream:
                info = os.fstat(stream.fileno())
                written[path] = (info.st_dev, info.st_ino)
                stream.write(data)
                stream.flush()
                os.fsync(stream.fileno())
            if name.startswith("fab-rs") and not name.endswith(".exe"):
                path.chmod(0o755)
            require(read(path, BINARY_CAP) == data, "Staged bytes changed")
        require({p.name for p in stage.iterdir()} == set(files), "Stage inventory drift")
        stage.rename(output / "bundle")
        return output / "bundle"
    except BaseException:
        # Never recursively remove a root that may contain another actor's files.
        for path, identity in written.items():
            try:
                info = plain(path)
                if (info.st_dev, info.st_ino) == identity:
                    path.unlink()
            except (OSError, AssemblyError):
                pass
        for path in (stage, output):
            try:
                path.rmdir()
            except OSError:
                pass
        raise


def assemble(expected: Path, expected_sha256: str, build_record: Path,
             cli: Path, helper: Path, output: Path, now: int,
             dirty_manifest: Path | None = None) -> dict[str, Any]:
    """Create an unsigned engineering assembly; do not install or execute it."""
    e, raw = load_expected(expected, expected_sha256)
    check_time(e, now)
    files = payload(e, raw, read(build_record), read(cli, BINARY_CAP),
                    read(helper, BINARY_CAP),
                    read(dirty_manifest) if dirty_manifest is not None else None)
    bundle = output_bundle(output, files)
    return {"schema": 1, "status": "unsigned-engineering-candidate",
            "publishable": False, "assembly_sha256": sha(files[ASSEMBLY_NAME]),
            "bundle": str(bundle), "blockers": preflight(e, None)}


def bundle_files(bundle: Path, e: dict[str, Any], raw: bytes,
                 signed: bool = False) -> dict[str, bytes]:
    absolute(bundle)
    plain(bundle, True)
    entries = list(bundle.iterdir())
    require(len(entries) <= 12, "Excess package entries")
    actual: dict[str, bytes] = {}
    binary_names = {names(e)[0], names(e)[1]}
    for path in entries:
        require(path.name.casefold() not in {n.casefold() for n in actual},
                "Duplicate package leaves")
        actual[path.name] = read(path, BINARY_CAP if path.name in binary_names
                                 else METADATA_CAP)
    for name in (*binary_names, BUILD_NAME, ASSEMBLY_NAME):
        require(name in actual, "Missing package entry")
    expected = payload(e, raw, actual[BUILD_NAME], actual[names(e)[0]],
                       actual[names(e)[1]], actual.get(DIRTY_NAME))
    signed_names = {ASSEMBLY_NAME + ".sig", names(e)[2] + ".sig",
                    "os-attestation-v1.json"} if signed else set()
    require(set(actual) == set(expected) | signed_names, "Package inventory mismatch")
    for name, data in expected.items():
        # Assembly-tool JSON is canonical; signatures cover these exact bytes.
        require(actual[name] == data, "Package content or provenance drift")
    return actual


def verify_candidate(bundle: Path, expected: Path, expected_sha256: str,
                     now: int) -> dict[str, Any]:
    """Reconstruct and compare the entire unsigned inventory from external pins."""
    e, raw = load_expected(expected, expected_sha256)
    check_time(e, now)
    files = bundle_files(bundle, e, raw)
    return {"schema": 1, "status": "unsigned-engineering-candidate",
            "publishable": False, "assembly_sha256": sha(files[ASSEMBLY_NAME]),
            "blockers": preflight(e, None)}


def load_policy(path: Path, e: dict[str, Any]) -> dict[str, Any]:
    raw = read(path)
    require(e["signing_policy_sha256"] is not None and
            sha(raw) == e["signing_policy_sha256"], "Signing policy pin mismatch")
    p = keys(parse(raw), POLICY_FIELDS)
    schema(p)
    digest(p["public_key_hex"], True)
    require(p["public_key_hex"] == e["public_key_hex"], "Publisher key policy mismatch")
    for field in POLICY_FIELDS - {"schema", "public_key_hex", "verifier"}:
        if p[field] is not None:
            text(p[field])
    if p["verifier"] is not None:
        v = keys(p["verifier"], {
            "kind", "path", "sha256", "bytes", "version", "approval_ref",
            "windows_system_root",
        })
        require(v["kind"] == "openssl-ed25519-v1", "Unsupported signature verifier")
        digest(v["sha256"])
        require(type(v["bytes"]) is int and 0 < v["bytes"] <= BINARY_CAP,
                "Invalid verifier length")
        require(isinstance(v["path"], str), "Invalid verifier path")
        require(matches(r"OpenSSL 3\.[ -~]{1,240}", v["version"]),
                "Unsupported verifier version")
        text(v["approval_ref"])
        if v["windows_system_root"] is not None:
            installation_path(v["windows_system_root"], True)
    return p


def preflight(e: dict[str, Any], policy: dict[str, Any] | None) -> list[str]:
    """Return all missing publisher inputs without inventing identities."""
    missing: list[str] = []
    if e["dirty_manifest_sha256"] is not None:
        missing.append("clean-reviewed-source-required")
    if "self-update" not in e["explicit_features"]:
        missing.append("self-update-feature-required-for-production-product")
    if e["public_key_hex"] is None:
        missing.append("independently-pinned-compiled-publisher-public-key")
    if e["signing_policy_sha256"] is None or policy is None:
        missing.append("approved-public-signer-policy")
    if e["os_attestation_sha256"] is None:
        missing.append("independently-pinned-native-os-signature-attestation")
    required = ["public_key_policy_ref", "native_signer_policy_ref",
                "native_verification_policy_ref", "production_workflow_ref",
                "release_authorization_ref", "verifier"]
    operating_system = e["platform"]["os"]
    if operating_system == "windows":
        required.append("windows_signing_profile_ref")
    elif operating_system == "macos":
        required.extend(("apple_developer_id_ref", "apple_notarization_policy_ref"))
    else:
        required.append("linux_signer_ref")
    for field in required:
        if policy is None or policy[field] is None:
            missing.append("missing-" + field.replace("_", "-"))
    return missing


def validate_attestation(raw: bytes, e: dict[str, Any], p: dict[str, Any]) -> None:
    require(sha(raw) == e["os_attestation_sha256"], "OS attestation digest mismatch")
    a = keys(parse(raw), {
        "schema", "target", "version", "tag", "policy_sha256",
        "verification_tool_ref", "evidence_ref", "files",
    })
    schema(a)
    require(a["target"] == e["target"] and a["version"] == e["version"]
            and a["tag"] == e["tag"] and a["policy_sha256"] ==
            e["signing_policy_sha256"], "OS attestation binding mismatch")
    text(a["verification_tool_ref"])
    text(a["evidence_ref"])
    require(type(a["files"]) is list and len(a["files"]) == 2,
            "OS attestation must cover both binaries")
    operating_system = e["platform"]["os"]
    kind, signer = {
        "windows": ("authenticode", "windows_signing_profile_ref"),
        "macos": ("developer-id-notarized", "apple_developer_id_ref"),
        "linux": ("gpg-detached", "linux_signer_ref"),
    }[operating_system]
    for role, item in zip(("cli", "helper"), a["files"]):
        keys(item, {"role", "sha256", "bytes", "signature_kind", "signer_ref",
                    "timestamp_ref", "notarization_ref", "verified"})
        require(item["role"] == role and item["sha256"] ==
                e["artifacts"][role]["sha256"] and item["bytes"] ==
                e["artifacts"][role]["bytes"] and type(item["bytes"]) is int
                and item["signature_kind"] == kind and item["signer_ref"] == p[signer]
                and item["verified"] is True, "Unverified native signature attestation")
        text(item["timestamp_ref"])
        if operating_system == "macos":
            text(item["notarization_ref"])
        else:
            require(item["notarization_ref"] is None, "Unexpected notarization")


def execute(command: list[str], cwd: Path, system_root: str | None,
            log_name: str) -> bytes:
    """Run only the approved verifier; bound streams and retain no raw output."""
    environment = {"PATH": "", "HOME": str(cwd), "USERPROFILE": str(cwd),
                   "TMP": str(cwd), "TEMP": str(cwd), "TMPDIR": str(cwd),
                   "OPENSSL_CONF": os.devnull, "OPENSSL_MODULES": str(cwd)}
    if os.name == "nt":
        require(system_root is not None, "Verifier requires explicit Windows SystemRoot")
        absolute(Path(system_root))
        plain(Path(system_root), True)
        environment.update(SystemRoot=system_root, WINDIR=system_root)
    out, err = cwd / (log_name + ".out"), cwd / (log_name + ".err")
    owned: list[tuple[Path, tuple[int, int]]] = []
    try:
        with contextlib.ExitStack() as stack:
            streams = []
            for path in (out, err):
                stream = stack.enter_context(path.open("xb"))
                info = os.fstat(stream.fileno())
                owned.append((path, (info.st_dev, info.st_ino)))
                streams.append(stream)
            stdout, stderr = streams
            process = subprocess.Popen(
                command, cwd=cwd, env=environment, stdin=subprocess.DEVNULL,
                stdout=stdout, stderr=stderr, shell=False,
            )
            try:
                deadline = time.monotonic() + 15
                while True:
                    require(time.monotonic() < deadline, "Signature verifier timed out")
                    require(out.stat().st_size <= 65536 and err.stat().st_size <= 65536,
                            "Signature verifier output exceeded bound")
                    if process.poll() is not None:
                        break
                    time.sleep(0.01)
                require(process.returncode == 0, "Signature verification failed")
            finally:
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=5)
        require(out.stat().st_size <= 65536 and err.stat().st_size <= 65536,
                "Signature verifier output exceeded bound")
        return out.read_bytes()
    finally:
        for path, identity in owned:
            if path.exists():
                info = plain(path)
                if (info.st_dev, info.st_ino) == identity:
                    path.unlink()


def signatures(files: dict[str, bytes], e: dict[str, Any], p: dict[str, Any],
               workspace: Path, allow_verifier: bool) -> None:
    require(allow_verifier, "Explicit approved-verifier execution consent required")
    verifier = p["verifier"]
    executable = absolute(Path(verifier["path"]))
    require(not executable.is_relative_to(workspace), "Verifier cannot be an asset")
    verifier_raw = read(executable, BINARY_CAP)
    require(sha(verifier_raw) == verifier["sha256"]
            and len(verifier_raw) == verifier["bytes"], "Verifier custody drift")
    workspace.mkdir(mode=0o700)
    created: list[tuple[Path, tuple[int, int]]] = []
    try:
        key_path = workspace / "publisher-public-key.der"
        # RFC 8410 SubjectPublicKeyInfo, public Ed25519 key only.
        public_der = bytes.fromhex("302a300506032b6570032100" + e["public_key_hex"])
        with key_path.open("xb") as stream:
            info = os.fstat(stream.fileno())
            created.append((key_path, (info.st_dev, info.st_ino)))
            stream.write(public_der)
        system_root = verifier["windows_system_root"]
        actual_version = execute([str(executable), "version"], workspace,
                                 system_root, "version")
        version_bytes = verifier["version"].encode("ascii")
        require(actual_version in (version_bytes, version_bytes + b"\n",
                                   version_bytes + b"\r\n"),
                "Approved verifier version mismatch")
        for index, name in enumerate((ASSEMBLY_NAME, names(e)[2])):
            message, signature = files[name], files[name + ".sig"]
            require(len(signature) == 64, "Ed25519 signature must be 64 raw bytes")
            message_path = workspace / f"message-{index}.json"
            signature_path = workspace / f"signature-{index}.sig"
            for path, data in ((message_path, message), (signature_path, signature)):
                with path.open("xb") as stream:
                    info = os.fstat(stream.fileno())
                    created.append((path, (info.st_dev, info.st_ino)))
                    stream.write(data)
            execute([
                str(executable), "pkeyutl", "-verify", "-pubin",
                "-inkey", str(key_path), "-keyform", "DER", "-rawin",
                "-in", str(message_path), "-sigfile", str(signature_path),
            ], workspace, system_root, f"verify-{index}")
        require(read(executable, BINARY_CAP) == verifier_raw, "Verifier changed")
    finally:
        for path, identity in reversed(created):
            info = plain(path)
            if (info.st_dev, info.st_ino) == identity:
                path.unlink()
        workspace.rmdir()


def signed_operation(bundle: Path, expected: Path, expected_sha256: str, now: int,
                     policy_path: Path, workspace: Path, allow_verifier: bool,
                     output: Path | None = None,
                     update_signature: Path | None = None,
                     assembly_signature: Path | None = None,
                     os_attestation: Path | None = None) -> dict[str, Any]:
    """Verify signatures separately; optionally expose a new promoted assembly."""
    e, raw = load_expected(expected, expected_sha256)
    check_time(e, now)
    p = load_policy(policy_path, e)
    missing = preflight(e, p)
    if missing:
        return {"schema": 1, "status": "blocked", "publishable": False,
                "blockers": missing}
    files = bundle_files(bundle, e, raw, signed=output is None)
    if output is not None:
        require(all(v is not None for v in
                    (update_signature, assembly_signature, os_attestation)),
                "All external signatures and native attestation required")
        files[names(e)[2] + ".sig"] = read(update_signature, 64)
        files[ASSEMBLY_NAME + ".sig"] = read(assembly_signature, 64)
        files["os-attestation-v1.json"] = read(os_attestation)
    validate_attestation(files["os-attestation-v1.json"], e, p)
    absolute(workspace, False)
    require(not workspace.is_relative_to(bundle) and
            not bundle.is_relative_to(workspace), "Verifier workspace overlaps package")
    verifier_path = absolute(Path(p["verifier"]["path"]))
    require(not verifier_path.is_relative_to(bundle), "Asset-supplied verifier forbidden")
    require(not expected.is_relative_to(bundle) and not policy_path.is_relative_to(bundle),
            "Trust pins cannot be supplied by the package")
    signatures(files, e, p, workspace, allow_verifier)
    # Recheck custody after even approved external execution.
    require(load_expected(expected, expected_sha256)[1] == raw, "Expectations changed")
    load_policy(policy_path, e)
    rechecked = bundle_files(bundle, e, raw, signed=output is None)
    require(all(files[name] == value for name, value in rechecked.items()),
            "Package changed during signature verification")
    if output is not None:
        output_bundle(output, files)
    return {
        "schema": 1, "status": "verified-signed-assembly", "publishable": False,
        "assembly_sha256": sha(files[ASSEMBLY_NAME]),
        "signature_verifier_sha256": p["verifier"]["sha256"],
        "blockers": ["independent-platform-install-protection-and-GA-acceptance",
                     "separately-authorized-production-publication"],
    }


def main(argv: list[str] | None = None) -> int:
    """Emit bounded metadata-only reports; nonzero means blocked or rejected."""
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="mode", required=True)
    for mode in ("assemble", "verify", "promote", "verify-signed"):
        command = sub.add_parser(mode)
        command.add_argument("--expect", type=Path, required=True)
        command.add_argument("--expect-sha256", required=True)
        command.add_argument("--now", type=int, required=True)
        if mode == "assemble":
            for argument in ("build-record", "cli", "helper", "output"):
                command.add_argument("--" + argument, type=Path, required=True)
            command.add_argument("--dirty-manifest", type=Path)
        else:
            command.add_argument("--bundle", type=Path, required=True)
        if mode in ("promote", "verify-signed"):
            command.add_argument("--policy", type=Path, required=True)
            command.add_argument("--workspace", type=Path, required=True)
            command.add_argument("--allow-approved-verifier", action="store_true")
        if mode == "promote":
            for argument in ("output", "update-signature", "assembly-signature",
                             "os-attestation"):
                command.add_argument("--" + argument, type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        if args.mode == "assemble":
            report = assemble(args.expect, args.expect_sha256, args.build_record,
                              args.cli, args.helper, args.output, args.now,
                              args.dirty_manifest)
        elif args.mode == "verify":
            report = verify_candidate(args.bundle, args.expect, args.expect_sha256, args.now)
        else:
            report = signed_operation(
                args.bundle, args.expect, args.expect_sha256, args.now, args.policy,
                args.workspace, args.allow_approved_verifier,
                **({name: getattr(args, name) for name in
                    ("output", "update_signature", "assembly_signature", "os_attestation")}
                   if args.mode == "promote" else {}),
            )
        print(json.dumps(report, sort_keys=True, allow_nan=False))
        return 2 if report["status"] == "blocked" else 0
    except (AssemblyError, OSError, subprocess.SubprocessError) as error:
        diagnostic = str(error) if isinstance(error, AssemblyError) else "OS operation failed"
        print(json.dumps({"schema": 1, "status": "rejected", "publishable": False,
                          "diagnostic": diagnostic}), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
