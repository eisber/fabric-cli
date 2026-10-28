# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture source constant selection/scope without interpreting runtime aliases."""

from __future__ import annotations

import argparse
import contextlib
import copy
import json
from pathlib import Path
from typing import Any

from literal_parameter_reference import oracle

DEFAULTS = {
    "DEFAULT_GUID": "00000000-0000-0000-0000-000000000000",
    "VERSION": "synthetic-baseline",
    "API_FORMAT_MAPPING": {"Notebook": "old"},
    "ITEM_TYPE_TO_FILE": {"DataPipeline": "original.json"},
    "DATA_PIPELINE_CONTENT_FILE_JSON": "original.json",
    "BULK_ACCEPTED_ITEM_TYPES": ["Notebook"],
    "BULK_UNSUPPORTED_ITEM_TYPES": ["Report"],
    "BULK_ITEM_COUNT_LIMIT": 100,
    "SERIAL_ITEM_PUBLISH_ORDER": None,
    "EXCLUDE_PATH_REGEX_MAPPING": {},
    "PROPERTY_PATH_ATTR_MAPPING": {},
}


def capture(root: Path) -> dict[str, Any]:
    """Run actual get_config_value and config_overrides_scope on injected globals."""
    namespace, hashes = oracle.reference_namespace(root)
    if {
        name: oracle.version(name) for name in oracle.DEPENDENCIES
    } != oracle.DEPENDENCIES:
        raise ValueError("Pinned oracle dependencies required")
    namespace["contextlib"] = contextlib
    oracle.load_nodes(
        root / "src" / "fabric_cicd" / "_common" / "_config_utils.py",
        {"config_overrides_scope"},
        namespace,
    )
    constants = namespace["constants"]
    cases = []
    for name, section in [
        ("flat", {"DEFAULT_GUID": "11111111-1111-1111-1111-111111111111"}),
        ("null-skip", {"VERSION": None}),
        ("false-is-value", {"VERSION": False}),
        ("zero-is-value", {"BULK_ITEM_COUNT_LIMIT": 0}),
        ("empty-string-is-value", {"DEFAULT_GUID": ""}),
        ("direct-map-is-environments", {"API_FORMAT_MAPPING": {"Notebook": "one"}}),
        (
            "wrapped-runtime-map",
            {
                "API_FORMAT_MAPPING": {
                    "dev": {"Notebook": "one"},
                    "prod": {"Report": "two"},
                }
            },
        ),
        ("missing-environment", {"API_FORMAT_MAPPING": {"prod": {"Notebook": "one"}}}),
        ("null-environment", {"API_FORMAT_MAPPING": {"dev": None}}),
        ("empty-environment-map", {"API_FORMAT_MAPPING": {}}),
        ("feature-list-override", {"FEATURE_FLAG": {"dev": ["enable_bulk_publish"]}}),
        (
            "feature-map-override",
            {"FEATURE_FLAG": {"dev": {"enable_bulk_publish": False}}},
        ),
        (
            "item-file-map",
            {"ITEM_TYPE_TO_FILE": {"dev": {"DataPipeline": "changed.json"}}},
        ),
        (
            "file-alias-not-recomputed",
            {"DATA_PIPELINE_CONTENT_FILE_JSON": "changed.json"},
        ),
        ("bulk-alias-not-recomputed", {"BULK_UNSUPPORTED_ITEM_TYPES": ["Notebook"]}),
        (
            "serial-string-values-retained",
            {"SERIAL_ITEM_PUBLISH_ORDER": {"dev": {"1": "Notebook"}}},
        ),
        ("exclude-map", {"EXCLUDE_PATH_REGEX_MAPPING": {"dev": {"Notebook": "^skip"}}}),
        (
            "property-map",
            {
                "PROPERTY_PATH_ATTR_MAPPING": {
                    "dev": {"Lakehouse": {"sqlendpoint": False}}
                }
            },
        ),
    ]:
        for key, value in DEFAULTS.items():
            setattr(constants, key, copy.deepcopy(value))
        constants.FEATURE_FLAG = {"base"}
        selected = {}
        for key in section:
            value = namespace["get_config_value"](section, key, "dev")
            if value is not None:
                selected[key] = value
        effective = None
        scope_error = None
        try:
            with namespace["config_overrides_scope"](
                {"constants": copy.deepcopy(section)}, "dev"
            ):
                effective = {
                    key: copy.deepcopy(getattr(constants, key)) for key in DEFAULTS
                }
                if "FEATURE_FLAG" in selected:
                    effective["FEATURE_FLAG"] = copy.deepcopy(constants.FEATURE_FLAG)
        except (AttributeError, TypeError, ValueError) as error:
            scope_error = type(error).__name__
        cases.append(
            {
                "id": name,
                "constants": section,
                "environment": "dev",
                "selected": selected,
                "effective_before_cleanup": effective,
                "scope_cleanup_error": scope_error,
            }
        )
    return {
        "source": oracle.SOURCE,
        "sourceHashes": hashes,
        "dependencies": oracle.DEPENDENCIES,
        "defaults": DEFAULTS,
        "boundary": "Actual source selectors and scope with synthetic injected globals. Records selection and pre-cleanup values separately from cleanup failure; not full deployment or runtime enum validation.",
        "cases": cases,
    }


def main() -> None:
    """Capture once before Rust expectations; compare fixed data thereafter."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--capture", type=Path)
    args = parser.parse_args()
    result = json.loads(json.dumps(capture(args.reference_root)))
    if args.capture:
        with args.capture.open("x", encoding="utf-8", newline="\n") as file:
            json.dump(result, file, ensure_ascii=True, indent=2)
            file.write("\n")
    elif result != json.loads(
        Path(__file__)
        .with_name("selected_constants_source.json")
        .read_text(encoding="utf-8")
    ):
        raise AssertionError("Pinned constant selection differs from immutable receipt")
    print(json.dumps({"cases": len(result["cases"]), "status": "passed"}))


if __name__ == "__main__":
    main()
