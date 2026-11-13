# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Fetch one immutable public Fabric docs snapshot without image blobs."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import subprocess

REPOSITORY = "https://github.com/MicrosoftDocs/fabric-docs.git"
PINNED_REVISION = "3ae7afc73169ac57b874d5aee09217bb35a4f8fc"
PATTERNS = [
    "*.md", "*.yml", "*.yaml", "*.json", "*.csv", "*.ipynb", "*.ps1",
    "*.py", "*.sh", "*.toml", "*.txt",
    "/LICENSE", "/LICENSE-CODE", "/NOTICE", "/SECURITY", "/CODEOWNERS",
]


def prepare(output: Path, revision: str) -> dict[str, object]:
    """Create a new sparse checkout; never overwrite or delete an existing path."""
    if not output.is_absolute() or output.exists():
        raise ValueError("Output must be a new absolute directory")
    if re.fullmatch(r"[0-9a-f]{40}", revision) is None:
        raise ValueError("Revision must be a full lowercase Git commit")
    output.parent.mkdir(parents=True, exist_ok=True)

    def git(*args: str) -> str:
        result = subprocess.run(
            ["git", "-c", "core.longpaths=true", "-c", "core.autocrlf=false", *args],
            check=True, capture_output=True, text=True, timeout=600,
        )
        return result.stdout.strip()

    git("clone", "--filter=blob:none", "--no-checkout", "--depth", "1",
        REPOSITORY, str(output))
    git("-C", str(output), "sparse-checkout", "set", "--no-cone", *PATTERNS)
    if git("-C", str(output), "rev-parse", "HEAD") != revision:
        git("-C", str(output), "fetch", "--depth", "1", "origin", revision)
    git("-C", str(output), "checkout", "--detach", revision)
    actual = git("-C", str(output), "rev-parse", "HEAD")
    if actual != revision:
        raise ValueError("Checkout does not match the requested revision")
    markdown = sorted((output / "docs").rglob("*.md"))
    if not markdown:
        raise ValueError("Fabric docs snapshot contains no Markdown")
    for license_name in ("LICENSE", "LICENSE-CODE"):
        if not (output / license_name).is_file():
            raise ValueError(f"Missing required notice: {license_name}")
    return {
        "schema_version": 1, "repository": REPOSITORY, "commit": actual,
        "source": str(output), "markdown_files": len(markdown),
        "markdown_bytes": sum(path.stat().st_size for path in markdown),
        "strategy": "blob-filtered sparse checkout; no image paths selected",
    }


def main() -> None:
    """Prepare a pinned corpus and emit its actual acquisition receipt."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--revision", default=PINNED_REVISION)
    args = parser.parse_args()
    print(json.dumps(prepare(args.output, args.revision), indent=2))


if __name__ == "__main__":
    main()
