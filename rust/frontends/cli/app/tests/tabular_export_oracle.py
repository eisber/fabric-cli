# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Validate real offline config exports using independent PyArrow decoders."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
from typing import Any
import uuid

import pyarrow as pa
import pyarrow.csv as csv
import pyarrow.parquet as parquet


def validate(binary: Path, artifact_root: Path) -> dict[str, Any]:
    """Run all three explicit exporters and retain their independently read proof."""
    binary = binary.resolve(strict=True)
    root = artifact_root.resolve() / uuid.uuid4().hex
    root.mkdir(parents=True)
    environment = os.environ.copy()
    environment["FAB_RUST_CONFIG_ROOT"] = str(root / "isolated-config")
    environment["FAB_RUST_AUTH_MODE"] = "deliberately-invalid"
    environment["FAB_RUST_TENANT"] = "deliberately-invalid"
    receipts: dict[str, Any] = {}
    for output_format in ("json", "csv", "parquet"):
        destination = root / f"config.{output_format}"
        command = [
            str(binary),
            "config",
            "ls",
            "--format",
            output_format,
            "--output",
            str(destination),
        ]
        result = subprocess.run(
            command,
            env=environment,
            capture_output=True,
            check=False,
            timeout=30,
        )
        if result.returncode:
            raise RuntimeError(
                f"{output_format} failed ({result.returncode}): "
                f"{result.stderr.decode('utf-8', errors='replace')}"
            )
        if result.stderr:
            raise AssertionError(f"{output_format} emitted unexpected diagnostics")
        receipt = json.loads(result.stdout)
        assert receipt["source"] == "config ls"
        assert receipt["execution"]["status"] == "succeeded"
        assert receipt["delivery"]["status"] == "succeeded"
        assert receipt["delivery"]["format"] == output_format
        assert receipt["delivery"]["bytes"] == destination.stat().st_size
        receipts[output_format] = receipt

    json_rows = json.loads((root / "config.json").read_bytes())
    csv_table = csv.read_csv(
        root / "config.csv",
        convert_options=csv.ConvertOptions(
            column_types={"setting": pa.string(), "value": pa.string()},
            strings_can_be_null=False,
        ),
    )
    parquet_table = parquet.read_table(root / "config.parquet")
    assert parquet_table.schema.names == ["setting", "value"]
    assert all(field.type == pa.string() for field in parquet_table.schema)
    assert parquet_table.schema.field("value").metadata[
        b"fabric.tabular.logical_type"
    ] == b"json"
    csv_rows = [
        {**row, "value": json.loads(row["value"])} for row in csv_table.to_pylist()
    ]
    parquet_rows = [
        {**row, "value": json.loads(row["value"])} for row in parquet_table.to_pylist()
    ]
    assert json_rows == csv_rows == parquet_rows
    assert json_rows[0] == {"setting": "cache_enabled", "value": "true"}
    for receipt in receipts.values():
        assert receipt["delivery"]["rows"] == len(json_rows)
        assert [column["name"] for column in receipt["delivery"]["schema"]] == [
            "setting",
            "value",
        ]
        assert [column["type"] for column in receipt["delivery"]["schema"]] == [
            "string",
            "json",
        ]
    proof = {
        "operation": "config ls",
        "binary": str(binary),
        "artifacts": str(root),
        "pyarrow_version": pa.__version__,
        "rows": len(json_rows),
        "parquet_schema": str(parquet_table.schema),
        "receipts": receipts,
    }
    (root / "proof.json").write_text(json.dumps(proof, indent=2), encoding="utf-8")
    return proof


def main() -> None:
    """Parse explicit binary/artifact inputs without authentication or live calls."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument(
        "--artifact-root",
        type=Path,
        default=Path("target") / "tabular-export-oracle",
    )
    arguments = parser.parse_args()
    print(json.dumps(validate(arguments.binary, arguments.artifact_root), indent=2))


if __name__ == "__main__":
    main()
