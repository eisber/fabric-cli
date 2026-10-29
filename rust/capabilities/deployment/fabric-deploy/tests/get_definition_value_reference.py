# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Run unchanged pinned GET value functions with explicit offline dependencies."""

from __future__ import annotations

import ast
import base64
from copy import deepcopy
from enum import Enum
from functools import cache
import hashlib
import json
import math
from pathlib import Path
import subprocess
import sys
from types import SimpleNamespace
from typing import Any

PIN = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
ROOT = Path(__file__).resolve().parents[5]
DIGESTS: dict[str, dict[str, Any]] = {}
RECEIPT = Path(__file__).with_suffix(".json")


@cache
def source(path: str) -> str:
    """Read the Git object, never import product code or discover credentials."""
    data = subprocess.check_output(
        ["git", "show", f"{PIN}:src/fabric_cli/{path}"], cwd=ROOT, timeout=10
    )
    current = (ROOT / "src" / "fabric_cli" / path).read_bytes()
    DIGESTS[path] = {
        "pinned_sha256": hashlib.sha256(data).hexdigest(),
        "checkout_sha256": hashlib.sha256(current).hexdigest(),
        "checkout_matches_pin": current == data,
    }
    return data.decode("utf-8")


def load(path: str, names: set[str], scope: dict[str, Any]) -> SimpleNamespace:
    """Execute whole unchanged functions/classes/mappings, not rewritten leaves."""
    nodes = [
        node for node in ast.parse(source(path)).body
        if (
            isinstance(node, (ast.FunctionDef, ast.ClassDef)) and node.name in names
        ) or (
            isinstance(node, ast.Assign)
            and any(isinstance(target, ast.Name) and target.id in names
                    for target in node.targets)
        )
    ]
    found = {
        node.name if isinstance(node, (ast.FunctionDef, ast.ClassDef))
        else node.targets[0].id for node in nodes
    }
    assert found == names, (path, found, names)
    tree = ast.Module(body=[
        ast.ImportFrom(module="__future__", names=[ast.alias(name="annotations")], level=0),
        *nodes,
    ], type_ignores=[])
    ast.fix_missing_locations(tree)
    values = dict(scope)
    exec(compile(tree, f"{PIN}:{path}", "exec"), values)
    return SimpleNamespace(**values)


def normalize(value: Any) -> Any:
    """Test-only explicit tags; these are not a production GET output envelope."""
    if isinstance(value, bytes):
        return {"$bytes": list(value)}
    if isinstance(value, float) and not math.isfinite(value):
        return {"$python": json.dumps(value)}
    if isinstance(value, int) and not isinstance(value, bool):
        if not -(2**63) <= value <= 2**64 - 1:
            return {"$python": str(value)}
    if isinstance(value, str) and any(0xD800 <= ord(char) <= 0xDFFF for char in value):
        return {"$python": json.dumps(value)}
    if isinstance(value, dict):
        if any(any(0xD800 <= ord(char) <= 0xDFFF for char in key) for key in value):
            return {"$python": json.dumps(value)}
        return {key: normalize(child) for key, child in value.items()}
    if isinstance(value, list):
        return [normalize(child) for child in value]
    return value


def part(text: str | bytes, path: str = "value.json", **extra: Any) -> dict[str, Any]:
    """Construct only synthetic public-format input."""
    raw = text.encode("utf-8") if isinstance(text, str) else text
    return {
        "path": path, "payload": base64.b64encode(raw).decode("ascii"),
        "payloadType": "InlineBase64", **extra,
    }


def cases() -> list[dict[str, Any]]:
    """Independent source inputs, including branches strict Rust must reject."""
    result: list[dict[str, Any]] = []

    def add(
        name: str, definition: Any, item_type: str = "Report", decode: bool = True,
        *, metadata: Any = None, strict: bool = False,
    ) -> None:
        result.append({
            "name": name,
            "metadata": metadata if metadata is not None else {
                "id": "synthetic-item", "type": item_type,
                "properties": {"specialized": "retained"}, "displayName": "Original",
            },
            "definition": definition, "item_type": item_type, "decode": decode,
            "strict_base64_exception": strict,
        })

    def single(name: str, payload: Any, **kw: Any) -> None:
        add(name, {"definition": {"parts": [payload]}}, **kw)

    add("metadata-only", None)
    add("missing-parts", {"definition": {"format": "native"}})
    add("empty-parts", {"definition": {"parts": []}})
    add("top-level-definitionParts", {"definitionParts": [part("false")]})
    add("parts-precedence", {
        "definition": {"parts": []}, "definitionParts": [part("true")],
    })
    add("fallback-definitionParts", {
        "definition": {}, "definitionParts": [part("true")],
    })
    add("ordered-merge", {"id": "overwritten", "first": 1, "definition": {"parts": []}},
        metadata={"first": 0, "id": "original", "properties": {"specialized": True}})
    add("order-paths-and-duplicate-paths", {"definition": {"parts": [
        part("first", "/../same.txt"), part("second", "/../same.txt"),
        part("third", "CON.txt"),
    ]}})
    for text in (
        "null", "true", "false", "0", "-0.0", "1.25", '"text"', '""',
        "[]", "{}", '{"z":1,"a":2,"z":3}', "[false,null,2]",
        "184467440737095516160123456789",
        '{"wide":-184467440737095516160123456789,"ok":2}',
        "NaN", "Infinity", "-Infinity", "1e999",
        r'"\ud800"', r'{"surrogate":"\udfff","astral":"\ud83d\ude00"}',
        r'{"\ud800":1}',
    ):
        single(f"json-{len(result)}", part(text))
    for path in ("v.json", "v.ipynb", "v.pbir", "v.platform", "v.pbism"):
        single(f"suffix-{path}", part('{"a":1}', path))
    for path in ("v.JSON", "v.json.txt", "", "model.tmdl", "env.yml", "source.py"):
        single(f"text-{path}", part("false", path))
    for text in (
        "", "{broken", "[1,]", '{"x":}', '{"x" 1}', '{"x":1,}', '"unterminated',
        '"bad\\q"', '"bad\\u12xz"', '"bad\nx"', "1.", "1e+", "01", "true false",
        "\ufeff{}", "  \n[1,\né]", '{"é":\n}', r'"\ud800\u000"',
    ):
        single(f"malformed-json-{len(result)}", part(text))
    single("binary-json", part(b"\xff\x00\r\n", "value.json"))
    single("binary-text", part(b"\xc0\x80", "value.txt"))
    single("utf8-text", part("é 😀\r\n", "value.txt"))
    single("utf8-bom-text", part("\ufefftext", "value.txt"))
    for payload in ("a", "ab", "abc", "abcde", "é", 1, True, 1.5, [1], {"a": 1}):
        single(f"per-part-error-{len(result)}", {"path": "v.json", "payload": payload, "error": "old"})
    for payload in ("", 0, False, None, [], {}):
        single(f"false-payload-{len(result)}", {"path": "v.json", "payload": payload, "payloadType": "Original"})
    single("missing-payload", {"path": "v.json", "payloadType": "Original"})
    single("payloadType-ignored", part("text", "v.txt", payloadType="NotBase64", error="old"))
    for path in (None, 42, False, [], {}):
        single(f"invalid-path-{len(result)}", {**part("text"), "path": path})
    single("missing-path", {"payload": "ZmFsc2U="})
    add("nested-source-selector", {"definition": {"parts": [{
        **part("outer", "o.txt"), "nested_parts": None,
        "definitionParts": [part('{"inner":true}')],
    }]}})
    add("nested-key-does-not-traverse-array", {"definition": {"parts": [{
        **part("outer", "o.txt"), "nested_parts": [part("true")],
    }]}})
    add("nested-without-marker-not-decoded", {"definition": {"parts": [{
        **part("outer", "o.txt"), "definitionParts": [part("true")],
    }]}})
    add("nested-recursive-definition", {"definition": {"parts": [{
        "nested_parts": False, "definition": {"parts": [{
            "nested_parts": [], "definitionParts": [part("null")],
        }]},
    }]}})
    notebook = {"definition": {"parts": [
        part('{"cells":[{"outputs":[1],"execution_count":7},{"source":["x"]}],"metadata":{"n":1}}', "n.ipynb"),
        part('{"cells":[{"outputs":[2]}]}', "second.ipynb"),
    ]}}
    add("notebook-first-existing-outputs-only", notebook, "Notebook")
    add("notebook-preserve", notebook, "Notebook", False)
    add("report-not-notebook-cleaning", notebook, "Report")
    single("notebook-source-format", part("print('x')", "notebook-content.py"), item_type="Notebook")
    for text in ("null", "true", '{"cells":null}', '{"cells":[null]}', '"cells"'):
        single(f"notebook-error-{len(result)}", part(text, "n.ipynb"), item_type="Notebook")
    single("notebook-binary-error", part(b"\xff"), item_type="Notebook")
    for text in (
        '{"cells":"abc"}', '{"cells":{"ordinary":1}}', '{"cells":["safe",[],{}]}',
        '{"cells":[]}', '[]', '"safe text"', '{"cells":[["outputs"]]}',
        '{"cells":["outputs"]}', '{"cells":{"outputs":1}}', '["cells"]',
    ):
        single(f"notebook-membership-{len(result)}", part(text, "n.ipynb"),
               item_type="Notebook")
    for item_type, path, text in (
        ("Report", "definition.pbir", '{"datasetReference":{"byPath":{"path":"../M"}}}'),
        ("SemanticModel", "definition/model.tmdl", "model Model\n\tculture: en-US"),
        ("SemanticModel", "model.bim", '{"model":{"tables":[]}}'),
        ("Environment", "environment.yml", "dependencies:\n  - package"),
        ("DataPipeline", "pipeline-content.json", '{"properties":{"activities":[]}}'),
        ("SparkJobDefinition", "SparkJobDefinitionV1.json", '{"mainDefinitionFile":"x.py"}'),
        ("VariableLibrary", "variables.json", '{"variables":[]}'),
        ("MirroredDatabase", "mirroring.json", '{"properties":{"source":{}}}'),
    ):
        single(f"type-{item_type}-{path}", part(text, path), item_type=item_type)
    for encoded in ("eA==%", "e%A==", "eA==\n", "Zh==", "eA===", "YQ=", "A==="):
        single(f"strict-policy-{len(result)}", {"path": "v.txt", "payload": encoded}, strict=True)
    add("preserve-noncanonical", {"definition": {"parts": [{"path": "v.txt", "payload": "eA==%"}]}},
        decode=False)
    for definition in ([], 1, "not-a-dictionary"):
        add(f"invalid-acquisition-{len(result)}", definition)
    add("invalid-parts", {"definition": {"parts": None}})
    return result


class SourceError(Exception):
    """Only the explicit source status-code contract, not product initialization."""

    def __init__(self, *args: Any) -> None:
        super().__init__("synthetic source error")
        self.status_code = args[-1]


def capture() -> dict[str, Any]:
    """Execute source merge/export/decode/cleanup, with no writes or live clients."""
    types = load("core/fab_types.py", {
        "_BaseItemType", "ItemType", "definition_format_mapping",
    }, {"Enum": Enum})
    formats = load("utils/fab_item_util.py", {"resolve_definition_format"}, vars(types))
    utils = load("utils/fab_cmd_export_utils.py", {"decode_payload", "clean_notebook_cells"},
                 {"base64": base64, "json": json})
    records = []
    for case in cases():
        events: list[Any] = []

        def metadata(args: Any, item_uri: bool = False) -> SimpleNamespace:
            events.append(["metadata", item_uri])
            return SimpleNamespace(text=json.dumps(case["metadata"]))

        def definition(args: Any) -> SimpleNamespace:
            events.append(["definition", args.format])
            if case["definition"] is None:
                raise SourceError(403)
            return SimpleNamespace(text=json.dumps(case["definition"]))

        client = load("client/fab_api_item.py", {"get_item_withdefinition"}, {
            "get_item": metadata, "get_item_definition": definition, "json": json,
            "FabricCLIError": SourceError,
            "fab_constant": SimpleNamespace(ERROR_UNAUTHORIZED=401, ERROR_FORBIDDEN=403,
                                           ERROR_INVALID_DEFINITION_PAYLOAD=999),
            "ErrorMessages": SimpleNamespace(Client=SimpleNamespace(
                response_payload_not_dictionary=lambda: "not a dictionary")),
        })
        export = load("commands/fs/export/fab_fs_export_item.py", {"export_single_item"}, {
            "deepcopy": deepcopy, "fab_item_util": formats, "item_api": client,
            "utils_export": utils, "ItemType": types.ItemType,
        })
        item = SimpleNamespace(
            workspace=SimpleNamespace(id="synthetic-workspace"),
            id="synthetic-item", item_type=types.ItemType(case["item_type"]),
            path="/W.Workspace/Item", name="Item",
        )
        try:
            value = export.export_single_item(item, SimpleNamespace(force=True),
                                             do_export=False, decode=case["decode"], item_uri=True)
            case["expected"] = normalize(value)
            case["expected_key_order"] = list(value)
        except Exception as error:
            case["source_error"] = type(error).__name__
        case["events"] = events
        records.append(case)
    by_name = {case["name"]: case for case in records}
    assert by_name["metadata-only"]["expected"]["properties"] == {"specialized": "retained"}
    assert by_name["ordered-merge"]["expected_key_order"] == ["first", "id", "properties", "definition"]
    assert by_name["notebook-first-existing-outputs-only"]["expected"]["definition"]["parts"][0]["payload"]["cells"] == [
        {"outputs": [], "execution_count": 7}, {"source": ["x"]},
    ]
    assert by_name["notebook-first-existing-outputs-only"]["expected"]["definition"]["parts"][1]["payload"]["cells"][0]["outputs"] == [2]
    assert by_name["binary-json"]["expected"]["definition"]["parts"][0]["payload"] == {"$bytes": [255, 0, 13, 10]}
    assert by_name["payloadType-ignored"]["expected"]["definition"]["parts"][0] == {
        "path": "v.txt", "payload": "text", "payloadType": "DecodeBase64", "error": "old",
    }
    assert by_name["nested-key-does-not-traverse-array"]["expected"]["definition"]["parts"][0]["nested_parts"][0]["payload"] == "dHJ1ZQ=="
    return {
        "source": PIN,
        "runtime": {"implementation": sys.implementation.name, "version": sys.version.split()[0]},
        "digests": DIGESTS, "cases": records,
    }


def main() -> None:
    """Default is verification; creation is explicit and refuses replacement."""
    actual = capture()
    if sys.argv[1:] == ["--capture"]:
        with RECEIPT.open("x", encoding="utf-8", newline="\n") as output:
            output.write(json.dumps(actual, ensure_ascii=True, indent=2) + "\n")
    elif not sys.argv[1:]:
        expected = json.loads(RECEIPT.read_text(encoding="utf-8"))
        assert actual["source"] == expected["source"]
        assert actual["runtime"] == expected["runtime"]
        assert actual["cases"] == expected["cases"]
        for path, digest in actual["digests"].items():
            assert digest["pinned_sha256"] == expected["digests"][path]["pinned_sha256"]
    else:
        raise SystemExit("Use no arguments to verify, or --capture to create a NEW receipt")
    print(json.dumps({
        "source": PIN, "runtime": actual["runtime"],
        "cases": len(actual["cases"]), "digests": DIGESTS,
    }, indent=2))


if __name__ == "__main__":
    main()
