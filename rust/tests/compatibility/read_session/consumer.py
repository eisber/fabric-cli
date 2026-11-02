# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Assert offline executable composition, not live or full-command parity."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
from pathlib import Path
from typing import Any

ORIGIN = "https://api.fabric.microsoft.com/v1"
WORKSPACES = {
    "status": 200,
    "body": {"value": [{"id": "w", "displayName": "Demo", "type": "Workspace"}]},
}
ITEMS = {
    "status": 200,
    "body": {"value": [{"id": "n", "displayName": "Book", "type": "Notebook"}]},
}
EMPTY = {"status": 200, "body": {"value": []}}


def check(probe: Path) -> None:
    """Run real parser/session/SDK/HTTP/query/output in an offline subprocess."""
    cases: list[dict[str, Any]] = [
        {"command": ["ls", "/", "-q", "[].name"], "responses": [WORKSPACES]},
        {
            "command": ["dir", "Demo.Workspace", "-l"],
            "responses": [WORKSPACES, ITEMS],
        },
        {
            "command": ["get", "Demo.Workspace/Book.Notebook", "-q", "description"],
            "responses": [
                WORKSPACES,
                ITEMS,
                {
                    "status": 200,
                    "body": {
                        "id": "n",
                        "displayName": "Book",
                        "description": "real shared executor",
                    },
                },
            ],
        },
        {
            "command": ["ls", "."],
            "current": "/Demo.Workspace",
            "responses": [WORKSPACES, ITEMS],
        },
        {
            "command": ["ls", "~"],
            "home": "Demo.Workspace",
            "responses": [WORKSPACES, ITEMS],
        },
        {
            "command": ["get", "Demo.Workspace", "-q", "sparkSettings.enabled"],
            "responses": [
                WORKSPACES,
                {"status": 200, "body": {"id": "w", "displayName": "Demo"}},
                EMPTY,
                {"status": 200, "body": {"enabled": False}},
                EMPTY,
            ],
        },
        {
            "command": ["ls", "/", "-la"],
            "responses": [WORKSPACES, EMPTY],
        },
        {
            "command": ["get", "Demo.Workspace"],
            "responses": [{"status": 401, "body": {"message": "never echo this"}}],
        },
        {
            "command": ["ls", "Demo.Workspace"],
            "responses": [
                {
                    "status": 200,
                    "body": {
                        **WORKSPACES["body"],
                        "continuationToken": "next",
                    },
                },
                {"status": 200, "body": {"invalid": []}},
            ],
        },
        {"command": ["ls", "/"], "cancelled": True},
        {
            "command": ["ls", "/", "--output_format", "text"],
            "responses": [WORKSPACES],
        },
        {
            "command": ["ls", "Demo.Workspace/Parent.Folder/Child.Folder", "-l"],
            "responses": [
                WORKSPACES,
                {
                    "status": 200,
                    "body": {
                        "value": [
                            {
                                "id": "child",
                                "displayName": "Child",
                                "parentFolderId": "parent",
                            },
                            {"id": "parent", "displayName": "Parent"},
                        ]
                    },
                },
                {
                    "status": 200,
                    "body": {
                        "value": [{**ITEMS["body"]["value"][0], "folderId": "child"}]
                    },
                },
                {
                    "status": 200,
                    "body": {
                        "value": [
                            {"id": "parent", "displayName": "Parent"},
                            {
                                "id": "child",
                                "displayName": "Child",
                                "parentFolderId": "parent",
                            },
                        ]
                    },
                },
            ],
        },
        {"command": ["ls", "/.capacities"]},
        {
            "command": ["ls", "/", "-l", "--output_format", "text"],
            "responses": [
                {
                    "status": 200,
                    "body": {
                        "value": [
                            {"id": "w", "displayName": "Caf\u00e9", "type": "Workspace"}
                        ]
                    },
                },
                EMPTY,
            ],
        },
        {
            "command": ["ls", "/", "-a", "--output_format", "text"],
            "responses": [WORKSPACES],
        },
    ]
    environment = {
        key: value
        for key, value in os.environ.items()
        if key.upper() in ("SYSTEMROOT", "WINDIR", "TEMP", "TMP")
    }
    observed = subprocess.run(
        [str(probe.resolve(strict=True))],
        input="".join(json.dumps(case) + "\n" for case in cases),
        text=True,
        encoding="utf-8",
        capture_output=True,
        check=True,
        timeout=60,
        env=environment,
    )
    assert observed.stderr == "", observed.stderr
    rows = [json.loads(line) for line in observed.stdout.splitlines()]
    assert len(rows) == len(cases)
    for index, row in enumerate(rows):
        assert (row["render_error"] is not None) == (index == 13), (index, row)
        assert bool(row["stderr"]) == (index == 14), (index, row)
        assert row["exit"] == (4 if index == 7 else 1 if index in (8, 9, 12, 13) else 0)
        assert all(
            scopes == ["https://api.fabric.microsoft.com/.default"]
            for scopes in row["scopes"]
        ), (index, row["scopes"])
    data = lambda index: json.loads(rows[index]["stdout"])["result"].get("data")
    assert data(0) == ["Demo.Workspace"]
    assert data(1) == [{"name": "Book.Notebook", "id": "n"}]
    assert data(2) == ["real shared executor"]
    assert rows[2]["requests"] == [
        f"{ORIGIN}/workspaces",
        f"{ORIGIN}/workspaces/w/items",
        f"{ORIGIN}/workspaces/w/notebooks/n",
    ]
    assert data(3) == data(4) == [{"name": "Book.Notebook"}]
    assert data(5) == ["False"]
    assert data(6)[0]["capacityName"] == "N/A"
    assert ".capacities" in rows[6]["stdout"]
    assert "never echo this" not in rows[7]["stdout"]
    assert data(8) is None
    assert len(rows[8]["requests"]) == 2
    assert rows[9]["requests"] == rows[9]["scopes"] == []
    assert rows[10]["stdout"] == "Demo.Workspace\n"
    assert data(11) == [{"name": "Book.Notebook", "id": "n"}]
    assert (
        rows[11]["requests"].count(f"{ORIGIN}/workspaces/w/folders?recursive=True") == 2
    )
    assert rows[12]["requests"] == rows[12]["scopes"] == []
    assert rows[13]["stdout"] == ""
    assert rows[13]["requests"] == [
        f"{ORIGIN}/workspaces",
        f"{ORIGIN}/capacities",
    ]
    assert ".capacities" in rows[14]["stdout"]
    assert rows[14]["stderr"].strip(), "text hidden separator must retain stderr"
    print(f"PASS {len(cases)} offline executable ReadSession compositions")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-probe", type=Path, required=True)
    check(parser.parse_args().rust_probe)
