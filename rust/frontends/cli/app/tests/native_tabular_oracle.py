# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Independently verify exports from the explicit offline command-host example."""

from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path
import subprocess
from typing import Any
import uuid

import pyarrow as pa
import pyarrow.parquet as parquet


EXPECTED = [
    {
        "signed": -(2**63),
        "unsigned": 2**64 - 1,
        "decimal": 1.5,
        "text": 'comma, "quotes"\r\n$HOME\\literal \u96ea',
        "active": True,
        "optional": None,
    },
    {
        "signed": 0,
        "unsigned": 1,
        "decimal": 0.0,
        "text": "",
        "active": False,
        "optional": "",
    },
]


def validate(example: Path, artifact_root: Path) -> dict[str, Any]:
    """Run actual parsers/hosts with a fixture and independently decode nine files."""
    example = example.resolve(strict=True)
    root = artifact_root.resolve() / uuid.uuid4().hex
    root.mkdir(parents=True)
    process = subprocess.run(
        [str(example), str(root)],
        capture_output=True,
        check=False,
        timeout=60,
    )
    if process.returncode:
        raise RuntimeError(
            f"Fixture failed ({process.returncode}): "
            f"{process.stderr.decode('utf-8', errors='replace')}"
        )
    if process.stderr:
        raise AssertionError("Fixture emitted unexpected diagnostics")
    manifest = json.loads(process.stdout)
    assert manifest["backend"] == "explicit-offline-fixture"
    assert manifest["service_calls"] == 0
    assert manifest["preparations"] == manifest["executions"] == 9
    assert len(manifest["results"]) == 9
    receipts = {
        (receipt["source"], receipt["delivery"]["format"]): receipt
        for receipt in manifest["results"]
    }
    assert len(receipts) == 9
    columns = list(EXPECTED[0])
    for command in ("run", "query", "execute"):
        paths = {
            encoding: root / f"{command}.{encoding}"
            for encoding in ("json", "csv", "parquet")
        }
        rows = json.loads(paths["json"].read_bytes())
        assert rows == EXPECTED
        assert all(list(row) == columns for row in rows)

        table = parquet.read_table(paths["parquet"])
        assert table.schema.names == columns
        assert table.schema.field("signed").type == pa.int64()
        assert table.schema.field("unsigned").type == pa.uint64()
        assert table.schema.field("decimal").type == pa.float64()
        assert table.schema.field("active").type == pa.bool_()
        assert table.schema.field("active").nullable
        assert table.schema.field("optional").type == pa.string()
        assert table.schema.field("optional").nullable
        assert table.to_pylist() == EXPECTED

        with paths["csv"].open(encoding="utf-8", newline="") as file:
            reader = csv.DictReader(file)
            assert reader.fieldnames == columns
            csv_rows = list(reader)
        assert len(csv_rows) == len(EXPECTED)
        for actual, expected in zip(csv_rows, EXPECTED, strict=True):
            for column, value in expected.items():
                if value is None:
                    text = ""
                elif isinstance(value, str):
                    text = value
                else:
                    text = json.dumps(value, separators=(",", ":"))
                assert actual[column] == text

        for encoding, destination in paths.items():
            receipt = receipts[command, encoding]
            assert receipt["execution"]["status"] == "succeeded"
            assert receipt["delivery"]["status"] == "succeeded"
            assert receipt["delivery"]["file"] == str(destination)
            assert receipt["delivery"]["rows"] == len(EXPECTED)
            assert receipt["delivery"]["bytes"] == destination.stat().st_size
            assert [
                column["name"] for column in receipt["delivery"]["schema"]
            ] == columns
    proof = {
        "example": str(example),
        "artifacts": str(root),
        "pyarrow_version": pa.__version__,
        "commands": ["run", "query", "execute"],
        "formats": ["csv", "json", "parquet"],
        "files_verified": 9,
        "rows_per_file": 2,
        "manifest": manifest,
    }
    (root / "proof.json").write_text(json.dumps(proof, indent=2), encoding="utf-8")
    return proof


def main() -> None:
    """Use only the explicitly supplied fixture executable and artifact directory."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--example", required=True, type=Path)
    parser.add_argument("--artifact-root", required=True, type=Path)
    arguments = parser.parse_args()
    print(json.dumps(validate(arguments.example, arguments.artifact_root), indent=2))


if __name__ == "__main__":
    main()
