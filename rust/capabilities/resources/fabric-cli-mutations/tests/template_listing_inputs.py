# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Independently authored walk INPUTS, never native-generated expected results."""

from __future__ import annotations

from copy import deepcopy
from pathlib import Path
from typing import Any, Iterator


LISTINGS = {
    "Blank.SemanticModel": [
        {"directory": "", "directories": ["definition"], "files": [".platform", "definition.pbism"]},
        {"directory": "definition", "directories": ["tables"], "files": ["model.tmdl", "database.tmdl"]},
        {"directory": "definition/tables", "directories": [], "files": ["Table.tmdl"]},
    ],
    "Blank.Report": [
        {"directory": "", "directories": ["StaticResources", "definition"], "files": ["definition.pbir", ".platform"]},
        {"directory": "StaticResources", "directories": ["SharedResources"], "files": []},
        {"directory": "StaticResources/SharedResources", "directories": ["BaseThemes"], "files": []},
        {"directory": "StaticResources/SharedResources/BaseThemes", "directories": [], "files": ["CY24SU10.json"]},
        {"directory": "definition", "directories": ["pages"], "files": ["version.json", "report.json"]},
        {"directory": "definition/pages", "directories": ["b8c5fb8d635f898326c6"], "files": ["pages.json"]},
        {"directory": "definition/pages/b8c5fb8d635f898326c6", "directories": [], "files": ["page.json"]},
    ],
    **{
        f"MirroredDatabase.{kind}": [
            {"directory": "", "directories": [], "files": ["mirroring.json"]}
        ]
        for kind in ["GenericMirror", "AzureSQLDatabase", "AzureSqlMI", "Snowflake", "CosmosDb"]
    },
}


def bind_listings(case: dict[str, Any]) -> None:
    """Bind identical explicit inputs before either actual consumer executes."""
    case.setdefault("template_enumeration", deepcopy(LISTINGS))


def walk(listings: dict[str, Any], directory: str) -> Iterator[tuple[str, list[str], list[str]]]:
    """Supply ordered rows to the unchanged pinned helper's os.walk boundary."""
    root = Path(directory)
    for row in listings[root.name]:
        yield str(root / row["directory"]), list(row["directories"]), list(row["files"])
