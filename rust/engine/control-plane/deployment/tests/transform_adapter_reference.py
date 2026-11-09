# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture extended actual Python regex/JSONPath semantics before native adapters."""

from __future__ import annotations

import argparse
import copy
import importlib.util
import json
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).parents[4]
spec = importlib.util.spec_from_file_location(
    "deploy_transform_adapter_oracle",
    ROOT / "tests" / "fixtures" / "deploy" / "reference_oracle.py",
)
oracle = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = oracle
spec.loader.exec_module(oracle)


def cases() -> list[dict[str, Any]]:
    """Declare synthetic inputs, never native-derived expected outputs."""
    result = []
    for name, pattern, content, replacement, flags in [
        ("unicode-capture", r"id=(\w+);", "id=caf\u00e9; id=\u03b1\u03b2;", "new", {}),
        (
            "unicode-ignore-case",
            "(i|s|k)",
            "I i \u0130 \u0131 S s \u017f K k \u212a",
            r"\1$1",
            {"ignore_case": True},
        ),
        (
            "one-capture-lookbehind",
            r"(?<=id=)(?P<value>\w+)(?=;)",
            "id=old; skip=old;",
            "new",
            {},
        ),
        ("pattern-backreference", r"(?P<value>a)(?P=value)", "aa ab aa", "x", {}),
        ("optional-first-missing", r"id=(x)?;", "id=; id=x;", "new", {}),
        ("optional-later-missing", r"id=(x)?;", "id=x; id=;", "new", {}),
        (
            "nested-noncapturing",
            r"(?:prefix)(old)(?:suffix)",
            "prefixoldsuffix prefixoldsuffix",
            r"\g<1>",
            {},
        ),
        ("inline-ignore-case", "(?i)(old)", "Old OLD old", "$literal", {}),
        ("filtered-invalid-regex", "(", "unchanged", "new", {"item_type": "Report"}),
        ("filtered-two-captures", "(a)(b)", "ab", "new", {"item_type": "Report"}),
        ("zero-width-whole-match", r"(?=(x))", "xx", "y", {}),
        ("dollar-absolute-end", r"(old)$", "old\n", "new", {}),
    ]:
        result.append(
            {
                "id": name,
                "operation": "parameters",
                "input": content,
                "parameters": {
                    "find_replace": [
                        {
                            "find_value": pattern,
                            "is_regex": True,
                            "replace_value": {"dev": replacement},
                            **flags,
                        }
                    ]
                },
            }
        )
    for name, expression, content, replacement in [
        (
            "numeric-filter-type-coercion",
            "$.items[?(@.n == 1)].value",
            '{"items":[{"n":1,"value":"a"},{"n":true,"value":"b"},{"n":"1","value":"c"}]}',
            "new",
        ),
        (
            "filter-missing-field",
            "$.items[?(@.n >= 2)].value",
            '{"items":[{"value":"missing"},{"n":2,"value":"two"},{"n":"3","value":"string"}]}',
            "new",
        ),
        (
            "bracket-property-special-characters",
            "$['a/b']['x.y']",
            '{"a/b":{"x.y":"old","~":"keep"}}',
            {"enabled": False},
        ),
        ("negative-index", "$.items[-1]", '{"items":[1,2,3]}', 7),
        ("slice-step", "$.items[::2]", '{"items":[0,1,2,3,4]}', 9),
        (
            "recursive-nested-array",
            "$..id",
            '{"id":"root","rows":[{"id":"a"},{"child":{"id":"b"}}]}',
            "new",
        ),
        (
            "wildcard-object-order",
            "$.*.value",
            '{"z":{"value":1},"a":{"value":2}}',
            "new",
        ),
        ("no-path-created", "$.missing.child", '{ "a": 1 }\r\n', "new"),
        ("leaf-null-replacement", "$.a", '{"a":1}', None),
        ("leaf-empty-string-replacement", "$.a", '{"a":1}', ""),
        ("json-root-replacement", "$", '{"a":1}', {"b": 2}),
        ("invalid-relative-path", "$.[", '{"a":1}', "new"),
    ]:
        result.append(
            {
                "id": name,
                "operation": "key_value",
                "input": content,
                "parameter": {
                    "find_key": expression,
                    "replace_value": {"dev": replacement},
                },
            }
        )
    result.append(
        {
            "id": "key-value-before-find-and-multiple-rule-order",
            "operation": "parameters",
            "input": '{"value":"source","second":"source"}',
            "parameters": {
                "find_replace": [
                    {"find_value": "middle", "replace_value": {"dev": "final"}},
                    {"find_value": "final", "replace_value": {"dev": "last"}},
                ],
                "key_value_replace": [
                    {"find_key": "$.value", "replace_value": {"dev": "middle"}},
                    {"find_key": "$.second", "replace_value": {"dev": "middle"}},
                ],
            },
        }
    )
    return result


def capture(root: Path) -> dict[str, Any]:
    """Execute pinned original leaves; failures remain observations, not repairs."""
    installed = {name: oracle.version(name) for name in oracle.DEPENDENCIES}
    if installed != oracle.DEPENDENCIES:
        raise ValueError(
            f"Requires pinned dependencies {oracle.DEPENDENCIES}; found {installed}"
        )
    namespace, hashes = oracle.reference_namespace(root)
    observed = []
    for case in cases():
        callbacks: list[str] = []
        try:
            output = {
                "value": oracle.evaluate(copy.deepcopy(case), namespace, callbacks)
            }
        except Exception as error:
            output = {"error": {"type": type(error).__name__, "message": str(error)}}
        observed.append({**case, "expected": output, "calls": callbacks})
    return {
        "source": oracle.SOURCE,
        "sourceHashes": hashes,
        "dependencies": installed,
        "boundary": "Actual pinned FabricWorkspace._replace_parameters, Python re and jsonpath_ng.ext path-mutation leaves. Leaf-only null/empty/regex-bool inputs are not full parameter-schema acceptance claims. Native adapters remain deferred/unimplemented.",
        "cases": observed,
    }


def main() -> None:
    """Capture once before native expectation, otherwise check immutable observations."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--capture", type=Path)
    args = parser.parse_args()
    result = capture(args.reference_root)
    if args.capture:
        with args.capture.open("x", encoding="utf-8", newline="\n") as file:
            json.dump(result, file, ensure_ascii=True, indent=2)
            file.write("\n")
    elif result != json.loads(
        Path(__file__)
        .with_name("transform_adapter_source.json")
        .read_text(encoding="utf-8")
    ):
        raise AssertionError(
            "Pinned transform adapter observations differ from immutable receipt"
        )
    print(
        json.dumps(
            {
                "source": oracle.CICD_COMMIT,
                "cases": len(result["cases"]),
                "status": "passed",
            }
        )
    )


if __name__ == "__main__":
    main()
