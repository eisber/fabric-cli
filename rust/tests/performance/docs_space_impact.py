# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Verify matched index/executable ablations and report only measured space impact."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any


PROFILES = {
    "baseline": dict(stemmed=False, metadata=False, hierarchy=False, autocomplete=False),
    "stemmed": dict(stemmed=True, metadata=False, hierarchy=False, autocomplete=False),
    "metadata": dict(stemmed=False, metadata=True, hierarchy=False, autocomplete=False),
    "hierarchy": dict(stemmed=False, metadata=False, hierarchy=True, autocomplete=False),
    "autocomplete": dict(stemmed=False, metadata=False, hierarchy=False, autocomplete=True),
    "full": dict(stemmed=True, metadata=True, hierarchy=True, autocomplete=True),
}


def digest(path: Path) -> str:
    """Hash the actual retained file rather than an estimate or a staging path."""
    checksum = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            checksum.update(chunk)
    return checksum.hexdigest()


def read(path: Path) -> dict[str, Any]:
    """Read one bounded, UTF-8 evidence object."""
    if not path.is_absolute():
        raise ValueError("Evidence paths must be absolute")
    with path.open("rb") as stream:
        raw = stream.read(16 * 1024 * 1024 + 1)
    if len(raw) > 16 * 1024 * 1024:
        raise ValueError("Evidence exceeds its byte budget")
    value = json.loads(raw.decode("utf-8-sig"))
    if not isinstance(value, dict):
        raise ValueError("Evidence must be an object")
    return value


def measure(request: dict[str, Any]) -> dict[str, Any]:
    """Verify all six isolated profiles, corpus/codec controls, and captured build identities."""
    if request.get("schema_version") != 1 or set(request.get("profiles", {})) != set(PROFILES):
        raise ValueError("Exactly baseline, four isolated additions, and full are required")
    rows = []
    control = None
    identity = None
    for name, features in PROFILES.items():
        entry = request["profiles"][name]
        manifest_path = Path(entry["bundle"]) / "bundle.json"
        manifest = read(manifest_path)
        if manifest.get("schema_id") != "fabric-docs-sections-v2" or manifest.get("features") != features:
            raise ValueError(f"{name}: index feature profile differs from its declared experiment")
        observed_control = {key: manifest[key] for key in (
            "schema_version", "schema_id", "tantivy_version", "source_commit",
            "source_repository", "source_markdown_files", "source_markdown_bytes", "documents",
            "compression", "segments", "unresolved_includes", "skipped_inputs",
        )}
        if control is None:
            control = observed_control
        elif control != observed_control:
            raise ValueError(f"{name}: corpus, codec, segment, or normalization controls changed")
        provenance = read(Path(entry["provenance"]))
        if provenance.get("profile") != "release" or provenance.get("index_manifest_sha256") != digest(manifest_path):
            raise ValueError(f"{name}: captured release/index identity is missing or stale")
        observed_identity = {key: provenance[key] for key in (
            "source_sha", "source_digest", "lockfile_sha256", "target_triple",
            "features", "default_features", "compiler",
        )}
        if identity is None:
            identity = observed_identity
        elif identity != observed_identity:
            raise ValueError(f"{name}: executable source, compiler, lock, or flags differ")
        binary = Path(provenance["path"])
        if not binary.is_absolute() or binary.stat().st_size != provenance["bytes"] or digest(binary) != provenance["sha256"]:
            raise ValueError(f"{name}: retained executable differs from captured artifact")
        receipt = read(Path(provenance["build_receipt"]))
        if type(receipt.get("exit")) is not int or receipt["exit"] != 0 or receipt.get("stopped") is not None:
            raise ValueError(f"{name}: unsuccessful build cannot establish a size impact")
        if receipt.get("source_unchanged") is not True or any(
            receipt.get(key) != provenance["source_digest"] for key in ("source_before", "source_after")
        ):
            raise ValueError(f"{name}: source changed during compilation")
        if digest(Path(provenance["build_receipt"])) != provenance["build_receipt_sha256"]:
            raise ValueError(f"{name}: build receipt changed after capture")
        probe_path = Path(provenance["corpus_probe"])
        if digest(probe_path) != provenance["corpus_probe_sha256"]:
            raise ValueError(f"{name}: executable corpus probe changed after capture")
        probe = read(probe_path)
        if probe.get("exit") != 0 or probe.get("stderr") != "" or probe.get("executable_sha256") != provenance["sha256"]:
            raise ValueError(f"{name}: corpus probe does not identify this successful executable")
        response = probe.get("response", {})
        if response.get("status") != "succeeded" or response.get("source") != "embeddedFabricDocs" or response.get("data") != manifest:
            raise ValueError(f"{name}: executable does not expose the claimed embedded bundle")
        if receipt.get("embedded_index") != str(manifest_path.parent):
            raise ValueError(f"{name}: build input is not the measured bundle")
        component_bytes = 0
        seen = set()
        for component in manifest["files"]:
            filename = component["name"]
            if filename in seen or not filename or "/" in filename or "\\" in filename or filename in (".", ".."):
                raise ValueError("Unsafe or duplicate index component")
            seen.add(filename)
            path = manifest_path.parent / filename
            if path.stat().st_size != component["bytes"] or digest(path) != component["sha256"]:
                raise ValueError(f"{name}: component does not match the finalized bundle")
            component_bytes += component["bytes"]
        rows.append({
            "feature": name, "index_component_bytes": component_bytes,
            "embedded_manifest_bytes": manifest_path.stat().st_size,
            "executable_bytes": provenance["bytes"],
            "manifest_sha256": digest(manifest_path), "executable_sha256": provenance["sha256"],
        })
    if len({row["executable_sha256"] for row in rows}) != len(rows):
        raise ValueError("Distinct populated index profiles cannot reuse the same executable capture")
    baseline = rows[0]
    for row in rows:
        row["additional_index_bytes"] = row["index_component_bytes"] - baseline["index_component_bytes"]
        row["additional_executable_bytes"] = row["executable_bytes"] - baseline["executable_bytes"]
    return {
        "schema_version": 1, "status": "passed", "identity": identity, "controls": control,
        "impact": rows, "interpretation": "Each isolated addition minus the same baseline; full is combined, not a sum. Actual compiled binary deltas include embedded metadata and linker/layout effects. No latency or relevance claim.",
    }


def measure_payload(request: dict[str, Any]) -> dict[str, Any]:
    """Measure exact embedded data only; never label this an executable-size delta."""
    if request.get("schema_version") != 1 or request.get("mode") != "embedded-payload":
        raise ValueError("Explicit embedded-payload measurement mode required")
    if set(request.get("profiles", {})) != set(PROFILES):
        raise ValueError("All six actual isolated/combined profiles are required")
    controls = None
    source = None
    rows = []
    for name, features in PROFILES.items():
        entry = request["profiles"][name]
        bundle = Path(entry["bundle"])
        manifest_path = bundle / "bundle.json"
        manifest = read(manifest_path)
        if manifest.get("schema_id") != "fabric-docs-sections-v2" or manifest.get("features") != features:
            raise ValueError("The actual bundle has the wrong index profile")
        observed = {key: manifest[key] for key in (
            "schema_version", "schema_id", "tantivy_version", "source_commit",
            "source_repository", "source_markdown_files", "source_markdown_bytes",
            "documents", "compression", "segments", "unresolved_includes", "skipped_inputs",
        )}
        if controls is None:
            controls = observed
        elif controls != observed:
            raise ValueError("Corpus/normalization/codec controls differ")
        receipt_path = Path(entry["index_receipt"])
        receipt = read(receipt_path)
        if type(receipt.get("exit")) is not int or receipt["exit"] != 0 or receipt.get("stopped") is not None or receipt.get("source_unchanged") is not True:
            raise ValueError("Missing or failed actual index-production receipt")
        if receipt["source_before"] != receipt["source_after"]:
            raise ValueError("Index production changed source")
        if source is None:
            source = receipt["source_after"]
        elif source != receipt["source_after"]:
            raise ValueError("Index producers do not share one source snapshot")
        argv = receipt["argv"]
        if "--profile" not in argv or argv[argv.index("--profile") + 1] != name:
            raise ValueError("Actual producer command has the wrong profile")
        if "--output" not in argv or argv[argv.index("--output") + 1] != str(bundle):
            raise ValueError("Actual producer output is not the measured bundle")
        components = 0
        names = set()
        for component in manifest["files"]:
            filename = component["name"]
            if not filename or filename in names or "/" in filename or "\\" in filename or filename in (".", ".."):
                raise ValueError("Unsafe/duplicate component name")
            names.add(filename)
            path = bundle / filename
            if path.stat().st_size != component["bytes"] or digest(path) != component["sha256"]:
                raise ValueError("Component differs from the finalized index")
            components += component["bytes"]
        rows.append({
            "feature": name, "index_component_bytes": components,
            "embedded_manifest_bytes": manifest_path.stat().st_size,
            "embedded_payload_bytes": components + manifest_path.stat().st_size,
            "manifest_sha256": digest(manifest_path),
            "index_receipt_sha256": digest(receipt_path),
        })
    for row in rows:
        row["additional_embedded_bytes"] = row["embedded_payload_bytes"] - rows[0]["embedded_payload_bytes"]
        row["additional_index_bytes"] = row["index_component_bytes"] - rows[0]["index_component_bytes"]
    return {
        "schema_version": 1, "status": "passed", "metric": "embedded-index-plus-manifest-bytes",
        "producer_source_digest": source, "controls": controls, "impact": rows,
        "executable_delta": "Not measured per feature. Payload bytes are included verbatim; linker layout, code and executable section alignment are not included.",
        "interpretation": "Each isolated addition minus one matched baseline; combined cost is not the sum. No latency or relevance inference.",
    }


def main() -> int:
    """Write a new verified impact receipt; never run builders or executables."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--request", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if not args.output.is_absolute() or args.output.exists():
        raise ValueError("Output must be a new absolute file")
    request = read(args.request)
    result = measure_payload(request) if request.get("mode") == "embedded-payload" else measure(request)
    with args.output.open("x", encoding="utf-8") as stream:
        json.dump(result, stream, indent=2)
        stream.write("\n")
    print(json.dumps({"status": result["status"], "impact": result["impact"]}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
