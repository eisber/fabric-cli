# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Check local identity evidence metadata; never authenticate or resolve evidence."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re
import sys
from typing import Any


MAX_BYTES = 256 * 1024
SCHEMA = Path(__file__).with_name("report.schema.json")
ROLES = {
    "release_owner",
    "identity_support_owner",
    "fabric_cli_team_reviewer",
    "environment_owner",
}


class ReportError(ValueError):
    """A fixed diagnostic code that contains no report values."""


def _require(condition: bool, code: str) -> None:
    if not condition:
        raise ReportError(code)


def _object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        _require(key not in result, "duplicate_json_key")
        result[key] = value
    return result


def _constant(_value: str) -> None:
    raise ReportError("nonfinite_json_number")


def load_json(path: Path) -> Any:
    """Read bounded UTF-8 JSON, rejecting duplicate keys and nonfinite numbers."""
    with path.open("rb") as stream:
        data = stream.read(MAX_BYTES + 1)
    _require(len(data) <= MAX_BYTES, "report_too_large")
    try:
        return json.loads(
            data.decode("utf-8"), object_pairs_hook=_object, parse_constant=_constant
        )
    except ReportError:
        raise
    except ValueError:
        raise ReportError("invalid_json") from None


def scope_digest(scope: dict[str, Any]) -> str:
    """Bind the complete exact scope with canonical ASCII JSON and SHA-256."""
    canonical = json.dumps(
        scope, sort_keys=True, separators=(",", ":"), ensure_ascii=True
    )
    return hashlib.sha256(canonical.encode("ascii")).hexdigest()


def packet_digest(report: dict[str, Any]) -> str:
    """Bind all report evidence and authorization, excluding signoffs themselves."""
    return scope_digest(
        {key: value for key, value in report.items() if key != "signoffs"}
    )


def _schema(value: Any, rule: dict[str, Any], definitions: dict[str, Any]) -> None:
    # This checker implements only the keywords used in the adjacent pinned schema.
    keywords = {
        "$schema",
        "$comment",
        "title",
        "$defs",
        "x-required-cases",
        "$ref",
        "type",
        "const",
        "enum",
        "pattern",
        "additionalProperties",
        "required",
        "properties",
        "minItems",
        "maxItems",
        "uniqueItems",
        "items",
    }
    _require(not (rule.keys() - keywords), "unsupported_schema_keyword")
    if "$ref" in rule:
        _require(set(rule) == {"$ref"}, "unsupported_schema_reference")
        reference = rule["$ref"]
        _require(reference.startswith("#/$defs/"), "unsupported_schema_reference")
        _schema(value, definitions[reference.removeprefix("#/$defs/")], definitions)
        return
    if "type" in rule:
        types = {
            "object": type(value) is dict,
            "array": type(value) is list,
            "string": type(value) is str,
            "boolean": type(value) is bool,
            "integer": type(value) is int,
            "null": value is None,
        }
        expected = rule["type"]
        expected = expected if isinstance(expected, list) else [expected]
        _require(any(types[kind] for kind in expected), "schema_type")
    if "const" in rule:
        _require(value == rule["const"], "schema_const")
    if "enum" in rule:
        _require(value in rule["enum"], "schema_enum")
    if isinstance(value, str) and "pattern" in rule:
        _require(re.fullmatch(rule["pattern"], value) is not None, "schema_pattern")
    if isinstance(value, dict):
        properties = rule.get("properties", {})
        _require(set(rule.get("required", [])) <= value.keys(), "schema_required")
        if rule.get("additionalProperties") is False:
            _require(value.keys() <= properties.keys(), "schema_unknown_field")
        for key in value.keys() & properties.keys():
            _schema(value[key], properties[key], definitions)
    if isinstance(value, list):
        _require(len(value) >= rule.get("minItems", 0), "schema_min_items")
        _require(len(value) <= rule.get("maxItems", MAX_BYTES), "schema_max_items")
        if rule.get("uniqueItems"):
            _require(
                len({json.dumps(item, sort_keys=True) for item in value}) == len(value),
                "schema_duplicate_item",
            )
        if "items" in rule:
            for item in value:
                _schema(item, rule["items"], definitions)


def _time(value: str) -> datetime:
    try:
        return datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(
            tzinfo=timezone.utc
        )
    except ValueError:
        raise ReportError("invalid_timestamp") from None


def _nonzero(value: str) -> bool:
    return any(character not in "0-" for character in value)


def validate(
    report: Any, *, synthetic_test_only: bool = False, now: datetime | None = None
) -> str:
    """Reject incomplete evidence metadata; return no authentication/approval claim."""
    schema = load_json(SCHEMA)
    _schema(report, schema, schema["$defs"])
    _require(
        report["synthetic_test_only"] == synthetic_test_only,
        "synthetic_mode_mismatch",
    )
    scope = report["scope"]
    digest = scope_digest(scope)
    if not synthetic_test_only:
        _require(
            "synthetic-" not in json.dumps(report),
            "synthetic_marker_in_live_report",
        )
    for field in (
        "tenant_id",
        "negative_tenant_id",
        "guest_home_tenant_id",
        "client_id",
        "arm_subscription_id",
        "source_sha",
        "primary_artifact_sha256",
        "negative_artifact_sha256",
    ):
        _require(_nonzero(scope[field]), "empty_target")
    for field in ("workspace_ids", "resource_ids"):
        _require(all(_nonzero(value) for value in scope[field]), "empty_target")
    _require(
        scope["primary_artifact_sha256"] != scope["negative_artifact_sha256"],
        "ambiguous_artifacts",
    )
    _require(
        scope["tenant_id"] != scope["negative_tenant_id"]
        and scope["tenant_id"] != scope["guest_home_tenant_id"]
        and len(
            {
                scope[field]
                for field in (
                    "account_ref",
                    "negative_account_ref",
                    "guest_account_ref",
                )
            }
        )
        == 3,
        "ambiguous_identity_targets",
    )
    _require("native-read" in scope["features"], "native_host_evidence_required")
    _require(int(scope["os_build"].split(".")[2]) >= 20348, "unsupported_os_build")
    authorization = report["authorization"]
    _require(authorization["status"] == "approved", "scope_not_approved")
    _require(authorization["scope_sha256"] == digest, "authorization_scope_mismatch")
    approved = _time(authorization["approved_at"])
    expires = _time(authorization["expires_at"])
    current = now or datetime.now(timezone.utc)
    _require(approved < expires and approved <= current, "invalid_approval_window")

    requirements = schema["x-required-cases"]
    _require(
        set(requirements) == set(schema["$defs"]["case_id"]["enum"]),
        "schema_case_inventory_mismatch",
    )
    cases = report["cases"]
    _require(
        len(cases) == len(requirements)
        and {case["id"] for case in cases} == set(requirements),
        "incomplete_or_duplicate_cases",
    )
    evidence = {entry["id"]: entry for entry in report["evidence"]}
    _require(len(evidence) == len(report["evidence"]), "duplicate_evidence_id")
    used = set()
    latest = approved
    for entry in evidence.values():
        _require(_nonzero(entry["sha256"]), "missing_evidence_digest")
        _require(entry["source_sha"] == scope["source_sha"], "stale_evidence_source")
        _require(entry["scope_sha256"] == digest, "evidence_scope_mismatch")
        artifact = (
            "negative_artifact_sha256"
            if entry["kind"] == "native_platform_negative"
            else "primary_artifact_sha256"
        )
        _require(
            entry["artifact_sha256"] == scope[artifact], "evidence_artifact_mismatch"
        )
        recorded = _time(entry["recorded_at"])
        _require(approved <= recorded <= current, "invalid_evidence_time")
    for case in cases:
        _require(case["status"] == "executed_pass", "unproved_case")
        _require(case["observation"] == "expected_observed", "unobserved_case")
        _require(case["executed_at"] is not None, "missing_execution_time")
        executed = _time(case["executed_at"])
        _require(
            approved <= executed <= min(expires, current), "unauthorized_execution"
        )
        latest = max(latest, executed)
        _require(bool(case["evidence_ids"]), "missing_case_evidence")
        for reference in case["evidence_ids"]:
            _require(reference in evidence, "unknown_evidence_id")
            entry = evidence[reference]
            _require(entry["kind"] == requirements[case["id"]], "wrong_evidence_kind")
            _require(case["id"] in entry["case_ids"], "evidence_case_mismatch")
            _require(
                executed <= _time(entry["recorded_at"]), "evidence_precedes_execution"
            )
            latest = max(latest, _time(entry["recorded_at"]))
            used.add(reference)
    _require(used == evidence.keys(), "unreferenced_evidence")
    signoffs = report["signoffs"]
    _require(
        len(signoffs) == len(ROLES) and {entry["role"] for entry in signoffs} == ROLES,
        "missing_or_duplicate_signoff",
    )
    for signoff in signoffs:
        _require(signoff["status"] == "approved", "signoff_pending")
        _require(signoff["scope_sha256"] == digest, "signoff_scope_mismatch")
        _require(
            signoff["packet_sha256"] == packet_digest(report), "signoff_packet_mismatch"
        )
        _require(
            latest <= _time(signoff["signed_at"]) <= current, "invalid_signoff_time"
        )
    return (
        "synthetic_test_only_not_acceptance"
        if synthetic_test_only
        else "metadata_complete_not_verified_not_release_approval"
    )


def main() -> int:
    """Validate one local report without reading any credential or network source."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument("--synthetic-test-only", action="store_true")
    arguments = parser.parse_args()
    try:
        result = validate(
            load_json(arguments.report),
            synthetic_test_only=arguments.synthetic_test_only,
        )
    except ReportError as error:
        print(f"identity_checklist_rejected: {error}", file=sys.stderr)
        return 1
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, RecursionError):
        print(
            "identity_checklist_rejected: unreadable_or_invalid_json", file=sys.stderr
        )
        return 1
    print(result)
    return 0


if __name__ == "__main__":
    sys.exit(main())
