# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Independent valid-metadata consumer denominator; no fixture recording mode.

Host-only validation, inside the shared OS mutex/private script:
  python -B rust\\tests\\compatibility\\workspace_get\\consumer_reference.py
  python -B -m unittest discover -s rust\\tests\\compatibility\\workspace_get -p test_consumer_reference.py -v

An explicit --rust-probe selects one unchanged existing JSONL adapter. All inputs
set raw_argv=true: read_probe safely ignores it, while folder_read_probe selects
its raw existing parser rather than legacy query joining. The host runs this
command separately for each probe and retains independent receipts. Source and
native expectations are manually authored before any native comparison. No
overlay is created for any of the original 70 cases.

String id/displayName fields are a mandatory domain requirement inherited from
the established ItemMetadata contract and the explicit task mandate, not an
inferred workspace-GET baseline or permission to weaken any original source
case. The sole malformed-primary negative is declared separately and never
counted as a nominal valid metadata object.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import sys
from pathlib import Path
from typing import Any

sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
SOURCE_COMMIT = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
INTEGRATION_COMMIT = "fad3ea7fee3a3e4e66205ffcf34d6b943bb416d7"
BASE_REFERENCE_BLOB = "2c2166f14b793e91b86bafe389fb3ce40a60cb50"
MANIFEST_BLOB = "7e7c3dd435dd241979da16c1cb6c3dcefd679a23"
IMMUTABLE_BASE_BLOBS = {
    "reference.py": BASE_REFERENCE_BLOB,
    "test_reference.py": "8ffbb7dc86fe9312cd70c4d40bf96dc9e84b4b23",
    "cases.json": "f63fd4307f8712504c23c38e381c6ea994696e9e",
    "status-cases.json": "fb605e652be16b395c59d51ff9f22b6726744276",
    "truthiness-cases.json": "3cf5447ad47fe7afd23080be37961e0de16d09b9",
    "README.md": "c2244a67620c7f65f7294162440a280be126968d",
    "requirements.txt": "731b164f490a4799a05b59476b658d5fc291abcc",
}
PRUNED_RESPONSE_COUNTS = {
    "consumer_primary_continuation": 1,
    "consumer_spark_continuation": 3,
    "consumer_foreign_mpe_continuation": 2,
    "consumer_trailing_201": 3,
    "consumer_trailing_204": 3,
}
SAFETY_IDS = (
    "consumer_primary_malformed",
    "consumer_primary_201",
    "consumer_primary_204",
    "consumer_primary_continuation",
    "consumer_spark_continuation",
    "consumer_foreign_mpe_continuation",
    "consumer_trailing_201",
    "consumer_trailing_204",
    "consumer_mpe_null_value",
    "consumer_acl_nonlist_value",
    "consumer_spark_null",
    "consumer_spark_array",
)
UNSUPPORTED_IDS = ("consumer_query_wide_sum", "consumer_query_big_literal")


def blob_identity(raw: bytes) -> str:
    """Normalize only checkout CRLF before computing the canonical Git blob."""
    raw = raw.replace(b"\r\n", b"\n")
    return hashlib.sha1(b"blob " + str(len(raw)).encode() + b"\0" + raw).hexdigest()


def _source_reference() -> Any:
    path = HERE / "reference.py"
    for name, expected in IMMUTABLE_BASE_BLOBS.items():
        if blob_identity((HERE / name).read_bytes()) != expected:
            raise AssertionError(f"Verified 70-case evidence changed: {name}")
    spec = importlib.util.spec_from_file_location("_consumer_workspace_source", path)
    if spec is None or spec.loader is None:
        raise AssertionError("Missing verified whole-command source runner")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


SOURCE = _source_reference()


def inputs_for(
    manifest: dict[str, Any], case: dict[str, Any], *, native: bool = False
) -> dict[str, Any]:
    """Use identical raw-parser JSONL inputs for both existing native consumers."""
    inputs = SOURCE.inputs_for(manifest, case, native=native)
    inputs["raw_argv"] = True
    return inputs


def verify_immutable_base() -> None:
    """Enforce the previous source runner and all three original fixture pins."""
    for name, expected in IMMUTABLE_BASE_BLOBS.items():
        if blob_identity((HERE / name).read_bytes()) != expected:
            raise AssertionError(f"Verified 70-case evidence changed: {name}")
    SOURCE.load_cases()
    SOURCE.load_status_cases()
    SOURCE.load_truthiness_cases()


def validate_manifest(manifest: dict[str, Any], raw: bytes) -> None:
    """Reject changed source/native contracts, valid-domain drift or hidden waivers."""
    if blob_identity(raw) != MANIFEST_BLOB or json.loads(raw) != manifest:
        raise AssertionError("Frozen consumer manifest changed")
    if (
        manifest["source_commit"] != SOURCE_COMMIT
        or manifest["integration_commit"] != INTEGRATION_COMMIT
        or manifest["base_reference_blob"] != BASE_REFERENCE_BLOB
        or manifest["schema_version"] != 1
        or manifest["_copyright"] != SOURCE.COPYRIGHT
    ):
        raise AssertionError("Consumer provenance changed")
    cases = manifest["cases"]
    identifiers = [case["id"] for case in cases]
    if (
        len(cases) != manifest["case_count"]
        or len(cases) != 58
        or len(set(identifiers)) != len(cases)
    ):
        raise AssertionError("Consumer denominator changed")
    if (
        tuple(case["id"] for case in cases if case["category"] == "native-safety")
        != SAFETY_IDS
    ):
        raise AssertionError("Consumer safety inventory changed")
    if (
        tuple(case["id"] for case in cases if case["category"] == "unsupported")
        != UNSUPPORTED_IDS
    ):
        raise AssertionError("Consumer unsupported inventory changed")
    for case in cases:
        if case["category"] not in {"exact", "native-safety", "unsupported"}:
            raise AssertionError("Unknown consumer classification")
        if not case.get("purpose") or not case.get("expected"):
            raise AssertionError("Missing independent source expectation")
        if case["category"] != "exact" and (
            not case.get("boundary") or not case.get("native_expected")
        ):
            raise AssertionError(
                "A native difference needs a complete separate contract"
            )
        inputs = inputs_for(manifest, case)
        primary = inputs["responses"][0]
        if primary["status"] in {200, 201}:
            body = primary["body"]
            if case["id"] == "consumer_primary_malformed":
                if primary != {"status": 200, "body": "not-json"}:
                    raise AssertionError(
                        "The declared malformed-primary negative changed"
                    )
            elif not (
                isinstance(body, dict)
                and isinstance(body.get("id"), str)
                and isinstance(body.get("displayName"), str)
            ):
                raise AssertionError(
                    "Nominal primary metadata must contain id/displayName"
                )
        if "native_input" in case:
            count = PRUNED_RESPONSE_COUNTS.get(case["id"])
            if (
                count is None
                or case["category"] != "native-safety"
                or set(case["native_input"]) != {"responses"}
                or case["native_input"]["responses"] != inputs["responses"][:count]
            ):
                raise AssertionError(
                    "Only declared unrequested reply suffixes may differ"
                )
        for native in (False, True):
            scripted = inputs_for(manifest, case, native=native)
            expected = SOURCE.expected_observation(manifest, case, native=native)
            if len(scripted["responses"]) != len(expected["requests"]):
                raise AssertionError(
                    "Each script must bind its full request denominator"
                )
    for case in cases:
        native = SOURCE.expected_observation(manifest, case, native=True)
        if case["id"] in {
            "consumer_primary_201",
            "consumer_primary_204",
            *PRUNED_RESPONSE_COUNTS,
        }:
            result = json.loads(native["stdout"])["result"]
            if (
                result
                != {
                    "error_code": "ReadFailed",
                    "message": "Workspace metadata operation did not complete",
                }
                or native["exit_code"] != 1
            ):
                raise AssertionError("Explicit standard fatal/status policy changed")
        if case["id"] == "consumer_primary_malformed":
            result = json.loads(native["stdout"])["result"]
            if result != {
                "error_code": "InvalidResponse",
                "message": "Fabric returned invalid workspace metadata",
            }:
                raise AssertionError("Explicit primary invalid-shape policy changed")
        if native["stderr"] != "":
            raise AssertionError("SDK diagnostics must not become extra CLI output")


def load_cases() -> dict[str, Any]:
    """Read independently authored valid-domain cases without running either engine."""
    verify_immutable_base()
    raw = (HERE / "consumer-cases.json").read_bytes()
    manifest = json.loads(raw)
    validate_manifest(manifest, raw)
    return manifest


def observe(
    manifest: dict[str, Any], case: dict[str, Any], sources: dict[str, Any]
) -> dict[str, Any]:
    """Execute the verified actual main/dispatch/workspace/API/HTTP/query source chain."""
    returned: list[Any] = []
    actual = SOURCE.observe(
        inputs_for(manifest, case), sources, returned, dispatch=True
    )
    if "returned" in case["expected"] and returned != [case["expected"]["returned"]]:
        raise AssertionError(f"{case['id']}: source workspace return changed")
    return actual


def main() -> None:
    """Check source and, only when requested by the host, actual native JSONL outcomes."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-probe", type=Path)
    parser.add_argument("--case")
    options = parser.parse_args()
    manifest = load_cases()
    cases = [case for case in manifest["cases"] if options.case in {None, case["id"]}]
    if not cases:
        parser.error("unknown frozen consumer case")
    sources = SOURCE.load_sources()
    original = []
    for case in cases:
        actual = observe(manifest, case, sources)
        SOURCE.assert_observation(manifest, case, actual)
        original.append(actual)
    if options.rust_probe:
        inputs = [inputs_for(manifest, case, native=True) for case in cases]
        native = SOURCE.COMMANDS.run_rust(options.rust_probe, inputs)
        counts = {"exact": 0, "native-safety": 0, "unsupported": 0}
        for case, reference, actual in zip(cases, original, native):
            SOURCE.assert_observation(manifest, case, actual, native=True)
            if case["category"] == "exact" and actual != reference:
                raise AssertionError("Exact consumer protocol differs from source")
            if case["category"] != "exact" and actual == reference:
                raise AssertionError(
                    "A declared consumer boundary is not source parity"
                )
            counts[case["category"]] += 1
        print(
            "Actual native consumer assertions, separately classified "
            f"(probe={options.rust_probe}): {counts}"
        )
    else:
        print("Native consumer outcomes NOT RUN; no native parity claim.")
    print(
        f"Checked {len(cases)} frozen valid-domain consumer source cases; "
        "previous 70 cases/fixtures/source runner remain unchanged. "
        f"Selection={options.case or 'all consumers'}; not full migration parity."
    )


if __name__ == "__main__":
    main()
