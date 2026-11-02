# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Independent pinned Python scalar database and real-source whole-text oracle.

Generation is additive and refuses existing destinations. Rust output is never
an input. The default smoke mode is bounded to fewer than 8192 scalars. Full
generation/verification additionally requires an explicit controller CPU slot.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib
import importlib.metadata
import json
import platform
import struct
import sys
import time
import unicodedata
from collections import Counter
from pathlib import Path
from typing import Any, Iterator

HERE = Path(__file__).resolve().parent
RUST = HERE.parents[4]
sys.path.insert(0, str(RUST / "tests" / "compatibility" / "output"))

from reference import BLOBS, SOURCE_COMMIT  # noqa: E402
from text_reference import TextReference  # noqa: E402

PYTHON_VERSION = "3.13.15"
PYTHON_UCD = "15.1.0"
RICH_VERSION = "14.3.3"
RICH_UCD = "17.0.0"
SCALAR_COUNT = 0x110000 - 0x800
EAW = ["N", "Na", "A", "H", "W", "F"]
CATEGORIES = [
    "Cn",
    "Lu",
    "Ll",
    "Lt",
    "Lm",
    "Lo",
    "Mn",
    "Me",
    "Mc",
    "Nd",
    "Nl",
    "No",
    "Zs",
    "Zl",
    "Zp",
    "Cc",
    "Cf",
    "Co",
    "Cs",
    "Pd",
    "Ps",
    "Pe",
    "Pc",
    "Po",
    "Sm",
    "Sc",
    "Sk",
    "So",
    "Pi",
    "Pf",
]
FNV_OFFSET = 0xCBF29CE484222325
FNV_PRIME = 0x100000001B3
MASK64 = (1 << 64) - 1
MAX_BUILD_SECONDS = 45
MAX_ARTIFACT_BYTES = 4 * 1024 * 1024


def canonical(value: Any) -> bytes:
    """Canonical JSON bytes for independently reproducible artifact hashes."""
    return json.dumps(value, ensure_ascii=True, separators=(",", ":")).encode("ascii")


def pinned_runtime() -> Any:
    """Fail closed on version drift and return explicitly versioned Rich cells."""
    if (
        platform.python_implementation() != "CPython"
        or platform.python_version() != PYTHON_VERSION
        or unicodedata.unidata_version != PYTHON_UCD
        or importlib.metadata.version("rich") != RICH_VERSION
    ):
        raise AssertionError("requires CPython 3.13.15/UCD 15.1.0 and Rich 14.3.3")
    from rich import cells

    if cells.load_cell_table(RICH_UCD).unicode_version != RICH_UCD:
        raise AssertionError("Rich did not select the exact requested Unicode table")
    return cells


def source_namespace() -> dict[str, Any]:
    """Use the existing hash-guarded isolated helper, not imported product globals."""
    return TextReference({"name": "unicode-source", "input": {}}).reference.namespace


def valid_scalars() -> Iterator[int]:
    """Enumerate all 1,112,064 scalars, without accepting surrogate code points."""
    yield from range(0xD800)
    yield from range(0xE000, 0x110000)


def is_case_ignorable(character: str) -> bool:
    """Observe Python's contextual sigma directly, including cased ignorables.

    The forward probe distinguishes a barrier from an ignorable/cased character.
    The backward probe then distinguishes non-ignorable cased characters from
    ignorables. No Rust classification or third-party regex UCD is involved.
    """
    return ("A\u03a3" + character + "A").lower()[1] == "\u03c3" and (
        " " + character + "\u03a3 "
    ).lower().endswith("\u03c3 ")


def scalar_record(point: int, namespace: dict[str, Any], cells: Any) -> list[int]:
    """Observe source width and actual CPython/Rich properties for one scalar."""
    character = chr(point)
    eaw = EAW.index(unicodedata.east_asian_width(character))
    expected_width = 2 if eaw >= 4 else 1
    if namespace["_char_visual_width"](character) != expected_width:
        raise AssertionError(f"source scalar width differs at U+{point:04X}")
    flags = (
        int(character.isprintable())
        | (int(character.isalnum()) << 1)
        | (int(character.isupper()) << 2)
        | (int(character.islower() or character.isupper() or character.istitle()) << 3)
        | (int(is_case_ignorable(character)) << 4)
        | (int(character.isspace()) << 5)
    )
    return [
        point,
        eaw,
        CATEGORIES.index(unicodedata.category(character)),
        flags,
        cells.get_character_cell_size(character, RICH_UCD),
    ]


def update_checksum(checksum: int, record: list[int]) -> int:
    """FNV-1a over LE u32 scalar followed by four property bytes."""
    for byte in struct.pack("<IBBBB", *record):
        checksum = ((checksum ^ byte) * FNV_PRIME) & MASK64
    return checksum


def sequences() -> list[str]:
    """Adversarial valid-scalar sequences, not an exhaustive grapheme claim."""
    return [
        "",
        "ASCII",
        "\0",
        "\x01\x1f\x7f\x80\x9f",
        "\t\r\n",
        "\x1c\x1d\x1e\x1f",
        " \u00a0\u1680\u2007\u2028\u2029\u202f\u3000",
        "\u00ad\u061c\u200b\u200c\u200d\u2060\ufeff",
        "\u0378\u0380\u1c89\u1fae",
        "\ufdd0\ufffe\uffff\U0010ffff",
        "\ue000\U000f0000\U00100000",
        "\u00e9",
        "e\u0301",
        "\u0301\u0308",
        "\u540d\u754c",
        "\uff21\uff61\u3000",
        "\u00a1\u00b7\u03a9",
        "\u2764",
        "\u2764\ufe0e",
        "\u2764\ufe0f",
        "\u2764\ufe0f\ufe0f",
        "\U0001f600",
        "\U0001fae8\U0001fae9",
        "\U0001f44d\U0001f3fd",
        "\U0001f1fa\U0001f1f8",
        "\U0001f1e6",
        "\U0001f469\u200d\U0001f4bb",
        "\U0001f468\u200d\U0001f469\u200d\U0001f467\u200d\U0001f466",
        "1\ufe0f\u20e3",
        "#\ufe0f\u20e3",
        "\ufe0f",
        "\u200d",
        "\u200dA",
        "A\u200d",
        "A\u200dB",
        "A\u200d\u200dB",
        "A\u0301\ufe0f",
        "\u2764\0\ufe0f",
        "\u2764\u200dA\ufe0f",
        "\u0915\u094d\u200d\u0937",
        "\u1100\u1161\u11a8",
        "\uac01",
        "\U0001f3f4\U000e0067\U000e0062\U000e007f",
        "\U000e0100",
        "'\"\\\x00\t\n\r",
        "\U00010000\U0010fffd",
        "A" * 511 + "\u2764\ufe0f",
        "A" * 512 + "\u200dB",
    ]


def whole_oracle(namespace: dict[str, Any], cells: Any) -> dict[str, Any]:
    """Capture original source functions and Python properties, never Rust values."""
    texts = []
    for text in sequences():
        texts.append(
            {
                "text": text,
                "source_width": namespace["_get_visual_length"](text),
                "rich_width": cells.cell_len(text, RICH_UCD),
                "repr": repr(text),
                "wraps": [
                    {"width": width, "lines": namespace["_wrap_text"](text, width)}
                    for width in (0, 1, 2, 3, 7)
                ],
            }
        )
    keys = [
        "",
        "_",
        " ",
        "name",
        "item_id",
        "cli_powerbi",
        "  name  ",
        "two words",
        "two_words",
        "two_ words",
        "camelCase",
        "ABC",
        "A_bc",
        "\u00e9tat_id",
        "\u00df_id",
        "\ufb03_id",
        "\u01c6_id",
        "\u0130_id",
        "\u03bf\u03a3_id",
        "\u03bf\u03a3\u03b1_id",
        "\u03b1\u0345\u03a3",
        "\u2160_id",
        "\u00b2_id",
        "\u00bd_id",
        "\u4e2d\u6587_id",
        "\u0301_id",
        "\u1c89_id",
        "\U00010428_id",
        "\u00aa_id",
        "\u03c2_id",
        "\u00e9tatId",
    ]
    key_cases = []
    for key in keys:
        try:
            result = {
                "expected": namespace["_format_key_to_convert_to_title_case"](key)
            }
        except ValueError as error:
            result = {"error": str(error)}
        key_cases.append({"key": key, **result})
    values: list[Any] = [
        None,
        True,
        False,
        0,
        -0.0,
        1.5,
        1e-7,
        1e16,
        "raw\u200b\0\u00a0",
        [],
        {},
        ["\u00e9", "\u200b", "\u00a0"],
        ["'", '"', "'\"\\\x00\t"],
        {"\u00e9": ["\u200d", "\U0010ffff", {"\U00010428": "\u2028"}]},
    ]
    for text in sequences():
        values.append([text])
    tables = []
    for index, text in enumerate(sequences()[:40]):
        case = {
            "name": f"unicode-table-{index}",
            "kind": "table",
            "width": (1, 2, 7, 12, 80)[index % 5],
            "input": {
                "data": [{"column": text}],
                "fields": ["column"],
                "show_headers": True,
            },
        }
        tables.append({"case": case, "expected": TextReference(case).observe()})
    render_inputs = [
        {
            "command": "get",
            "key_value": True,
            "data": {"\u00e9tat_id": ["\u00e9", "\u200b", "\u00a0"]},
        },
        {
            "command": "ls",
            "show_headers": True,
            "data": [{"\u00e9": ["\u00e9", "\u200b"], "\u540d": "\u5973\u200d"}],
        },
        {
            "command": "get",
            "key_value": True,
            "data": {"\u00e9tat_id": ["\u00e9", "\u200b"], "bad-key": 2},
        },
    ]
    render_cases = []
    for index, render_input in enumerate(render_inputs):
        case = {
            "name": f"unicode-default-render-{index}",
            "kind": "render",
            "width": 20,
            "input": render_input,
        }
        render_cases.append({"case": case, "expected": TextReference(case).observe()})
    return {
        "texts": texts,
        "keys": key_cases,
        "values": [{"value": value, "expected": str(value)} for value in values],
        "tables": tables,
        "render_cases": render_cases,
        "invalid_scalars": [0xD800, 0xDBFF, 0xDC00, 0xDFFF, 0x110000, 0xFFFFFFFF],
        "source_only_domain": {
            "surrogate_repr": repr("\ud800"),
            "surrogate_source_width": namespace["_char_visual_width"]("\ud800"),
            "native_policy": "reject non-scalars; do not broaden existing JSON contracts",
        },
    }


def casing_oracle(
    mappings: list[list[Any]], namespace: dict[str, Any]
) -> list[dict[str, str]]:
    """Run the actual source key function at every non-identity casing scalar."""
    cases = []
    for point, _, _ in mappings:
        character = chr(point)
        for key in (
            character + "_id",
            "a" + character + "_id",
            "a" + character + "a_id",
        ):
            try:
                result = {
                    "expected": namespace["_format_key_to_convert_to_title_case"](key)
                }
            except ValueError as error:
                result = {"error": str(error)}
            cases.append({"key": key, **result})
    return cases


def rich_source_hashes(cells: Any) -> dict[str, str]:
    """Record exact installed upstream code and selected data-file provenance."""
    modules = (
        cells,
        importlib.import_module("rich._unicode_data"),
        importlib.import_module("rich._unicode_data.unicode17-0-0"),
    )
    return {
        module.__name__: hashlib.sha256(Path(module.__file__).read_bytes()).hexdigest()
        for module in modules
    }


def build(
    namespace: dict[str, Any], cells: Any
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Perform the explicitly authorized full sweep; coalesce complete ranges."""
    deadline = time.monotonic() + MAX_BUILD_SECONDS
    ranges: list[list[int]] = []
    mappings = []
    counts: Counter[str] = Counter()
    checksum = FNV_OFFSET
    scalar_count = 0
    for point in valid_scalars():
        if scalar_count % 4096 == 0 and time.monotonic() > deadline:
            raise TimeoutError("full Python Unicode sweep exceeded its 45-second bound")
        record = scalar_record(point, namespace, cells)
        checksum = update_checksum(checksum, record)
        _, eaw, category, flags, rich_width = record
        counts["category:" + CATEGORIES[category]] += 1
        counts["eaw:" + EAW[eaw]] += 1
        counts["rich_width:" + str(rich_width)] += 1
        for index, name in enumerate(
            (
                "printable",
                "alphanumeric",
                "uppercase",
                "cased",
                "case_ignorable",
                "whitespace",
            )
        ):
            if flags & (1 << index):
                counts[name] += 1
        if ranges and ranges[-1][1] + 1 == point and ranges[-1][2:] == record[1:]:
            ranges[-1][1] = point
        else:
            ranges.append([point, point, eaw, category, flags, rich_width])
        character = chr(point)
        lower, title = character.lower(), character.title()
        if lower != character or title != character:
            mappings.append([point, lower, title])
        scalar_count += 1
    if scalar_count != SCALAR_COUNT:
        raise AssertionError("incomplete scalar domain")
    if (
        len(ranges) > 32768
        or len(mappings) > 8192
        or any(len(lower) > 3 or len(title) > 3 for _, lower, title in mappings)
        or len(cells.load_cell_table(RICH_UCD).narrow_to_wide) > 512
    ):
        raise AssertionError("generated tables exceed the reviewed provider bounds")
    data = {
        "schema": "fabric-output-unicode-v1",
        "source_commit": SOURCE_COMMIT,
        "python_version": PYTHON_VERSION,
        "python_ucd": PYTHON_UCD,
        "rich_version": RICH_VERSION,
        "rich_ucd": RICH_UCD,
        "scalar_count": scalar_count,
        "scalar_fnv64": f"{checksum:016x}",
        "ranges_sha256": hashlib.sha256(canonical(ranges)).hexdigest(),
        "copyright": "Copyright (c) Microsoft Corporation.",
        "licenses": ["MIT", "Unicode-3.0", "PSF-2.0"],
        "ranges": ranges,
        "case_mappings": mappings,
        "rich_narrow_to_wide": sorted(
            map(ord, cells.load_cell_table(RICH_UCD).narrow_to_wide)
        ),
    }
    oracle = {
        "copyright": "Copyright (c) Microsoft Corporation.",
        "licenses": data["licenses"],
        "source_commit": SOURCE_COMMIT,
        "source_blobs": BLOBS,
        "rich_source_sha256": rich_source_hashes(cells),
        "cpython_source": "https://github.com/python/cpython/tree/v3.13.15",
        "python_version": PYTHON_VERSION,
        "python_ucd": PYTHON_UCD,
        "rich_version": RICH_VERSION,
        "rich_ucd": RICH_UCD,
        "scalar_count": scalar_count,
        "coverage": dict(sorted(counts.items())),
        "data_sha256": hashlib.sha256(canonical(data)).hexdigest(),
        "casing_keys": casing_oracle(mappings, namespace),
        **whole_oracle(namespace, cells),
    }
    if time.monotonic() > deadline:
        raise TimeoutError(
            "full Python Unicode generation exceeded its 45-second bound"
        )
    return data, oracle


def smoke(namespace: dict[str, Any], cells: Any) -> None:
    """Run a bounded real-source check and estimate, not claim, full sweep cost."""
    points = list(range(4096))
    points.extend(sorted({ord(c) for text in sequences() for c in text} - set(points)))
    start = time.perf_counter()
    checksum = FNV_OFFSET
    for point in points:
        checksum = update_checksum(checksum, scalar_record(point, namespace, cells))
    elapsed = time.perf_counter() - start
    oracle = whole_oracle(namespace, cells)
    if namespace["_get_visual_length"]("e\u0301") != 2:
        raise AssertionError("source width must not become wcwidth")
    if cells.cell_len("e\u0301", RICH_UCD) != 1:
        raise AssertionError("Rich width must remain distinct")
    for character, expected in [
        ("A", False),
        ("\u0301", True),
        (" ", False),
        ("\u0345", True),
        ("'", True),
    ]:
        if is_case_ignorable(character) != expected:
            raise AssertionError("sigma probe failed")
    print(
        json.dumps(
            {
                "smoke_scalars": len(points),
                "complete_scalar_domain_tested": False,
                "seconds": round(elapsed, 4),
                "linear_sweep_estimate_seconds_not_a_measurement": round(
                    elapsed * SCALAR_COUNT / len(points), 2
                ),
                "whole_text_cases": len(oracle["texts"]),
                "key_cases": len(oracle["keys"]),
                "value_cases": len(oracle["values"]),
                "source_table_cases": len(oracle["tables"]),
                "python": PYTHON_VERSION,
                "python_ucd": PYTHON_UCD,
                "rich": RICH_VERSION,
                "rich_ucd": RICH_UCD,
            },
            sort_keys=True,
        )
    )


def main() -> None:
    """Smoke by default; generation and verification need the full-scalar slot."""
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--generate", action="store_true")
    mode.add_argument("--verify", action="store_true")
    parser.add_argument("--full-scalar-slot", action="store_true")
    args = parser.parse_args()
    cells = pinned_runtime()
    namespace = source_namespace()
    if not (args.generate or args.verify):
        smoke(namespace, cells)
        return
    if not args.full_scalar_slot:
        parser.error("full sweep requires the controller's explicit CPU slot")
    destinations = [
        HERE / "source-data.json",
        HERE / "oracle.json",
        HERE / "PYTHON-LICENSE.txt",
    ]
    if args.generate and any(path.exists() for path in destinations):
        raise AssertionError(
            "additive generation refuses existing artifact destinations"
        )
    if args.verify and not all(path.exists() for path in destinations):
        raise AssertionError(
            "generate and review all artifacts before full verification"
        )
    data, oracle = build(namespace, cells)
    python_license_source = (Path(sys.base_prefix) / "LICENSE.txt").read_bytes()
    if b"PYTHON SOFTWARE FOUNDATION LICENSE VERSION 2" not in python_license_source:
        raise AssertionError("missing complete Python license")
    python_license = (
        b"Copyright (c) Microsoft Corporation.\n"
        b"This notice collection preserves the unmodified third-party license below;\n"
        b"the following Python license, not Microsoft's MIT license, governs Python.\n\n"
        + python_license_source
    )
    notices = (HERE / "THIRD_PARTY_NOTICES.txt").read_bytes()
    oracle["notice_sha256"] = hashlib.sha256(notices).hexdigest()
    oracle["python_license_sha256"] = hashlib.sha256(python_license).hexdigest()
    oracle["python_license_source_sha256"] = hashlib.sha256(
        python_license_source
    ).hexdigest()
    if any(len(canonical(value)) > MAX_ARTIFACT_BYTES for value in (data, oracle)):
        raise AssertionError("generated artifact exceeds its 4-MiB bound")
    payloads = [canonical(data) + b"\n", canonical(oracle) + b"\n", python_license]
    if args.verify:
        for path, payload in zip(destinations, payloads):
            if path.read_bytes() != payload:
                raise AssertionError(f"independent Python/source mismatch: {path.name}")
    else:
        created: list[Path] = []
        try:
            for path, payload in zip(destinations, payloads):
                with path.open("xb") as stream:
                    created.append(path)
                    stream.write(payload)
        except BaseException:
            for path in created:
                path.unlink(missing_ok=True)
            raise
    print(
        json.dumps(
            {
                "mode": "verify" if args.verify else "generate",
                "scalar_count": SCALAR_COUNT,
                "ranges": len(data["ranges"]),
                "case_mappings": len(data["case_mappings"]),
                "scalar_fnv64": data["scalar_fnv64"],
                "data_bytes": len(canonical(data)),
                "data_sha256": hashlib.sha256(canonical(data)).hexdigest(),
            },
            sort_keys=True,
        )
    )


if __name__ == "__main__":
    main()
