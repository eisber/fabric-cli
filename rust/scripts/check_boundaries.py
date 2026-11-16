# Copyright (c) Microsoft Corporation. Licensed under the MIT License.
"""Offline metadata-only guard for the explicit reduced workspace; no compilation."""

import json
from pathlib import Path
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[1]
MEMBERS = {
    "engine/core/contracts": "fabric-core",
    "engine/core/json": "fabric-core-json",
    "engine/core/auth/provider": "fabric-identity",
    "engine/core/http/requests": "fabric-sdk-api",
    "engine/core/http/executor": "fabric-sdk-http",
    "engine/core/http/transport": "fabric-sdk-transport",
    "engine/core/lro": "fabric-sdk-operations",
    "engine/resources": "fabric-resources",
    "clients/cli/commands/api": "fabric-cli-api",
    "clients/cli/contracts": "fabric-cli-command-contracts",
    "clients/cli/output": "fabric-cli-output",
    "clients/cli/query": "fabric-cli-query",
    "clients/cli/app": "fabric-cli",
}
# Allowed internal edges. Includes all dependency kinds and target/optional edges.
EDGES = {
    "fabric-core": set(),
    "fabric-core-json": set(),
    "fabric-identity": {"fabric-core"},
    "fabric-sdk-http": {"fabric-core"},
    "fabric-sdk-transport": {"fabric-core", "fabric-sdk-http"},
    "fabric-sdk-operations": {"fabric-core", "fabric-core-json", "fabric-sdk-http"},
    "fabric-sdk-api": {
        "fabric-core", "fabric-core-json", "fabric-sdk-http", "fabric-sdk-operations",
    },
    "fabric-resources": {"fabric-core", "fabric-core-json", "fabric-sdk-api", "fabric-sdk-http"},
    "fabric-cli-command-contracts": {"fabric-core", "fabric-core-json"},
    "fabric-cli-query": {"fabric-core-json"},
    "fabric-cli-output": {"fabric-core", "fabric-core-json", "fabric-cli-command-contracts"},
    "fabric-cli-api": {
        "fabric-core", "fabric-core-json", "fabric-sdk-api", "fabric-sdk-http",
        "fabric-cli-command-contracts", "fabric-cli-output", "fabric-cli-query",
    },
    "fabric-cli": set(MEMBERS.values()) - {"fabric-cli"},
}
FORBIDDEN = ("broker", "updater", "pagination", "deployment", "azure-identity-broker")


def validate(metadata: dict, root: Path = ROOT) -> None:
    """Reject extra leaves, forbidden families, inverted edges and external paths."""
    root = root.resolve()
    manifest = tomllib.loads((root / "Cargo.toml").read_text())
    if set(manifest["workspace"]["members"]) != set(MEMBERS):
        raise ValueError("Workspace members must be the explicit retained leaves (no globs)")
    packages = {p["id"]: p for p in metadata["packages"]}
    actual = {}
    for package_id in metadata["workspace_members"]:
        package = packages[package_id]
        path = Path(package["manifest_path"]).resolve().parent.relative_to(root).as_posix()
        actual[path] = package["name"]
    if actual != MEMBERS:
        raise ValueError(f"Unexpected workspace package map: {actual}")
    graph = {}
    for package in packages.values():
        name = package["name"]
        if name in MEMBERS.values() and package["id"] not in metadata["workspace_members"]:
            raise ValueError(f"Retained package name used outside the workspace: {name}")
        if name.startswith(("fabric-", "fab-")) and name not in MEMBERS.values():
            raise ValueError(f"Excluded Fabric package: {name}")
        if any(word in name for word in FORBIDDEN):
            raise ValueError(f"Excluded dependency: {name}")
        if package["source"] is None and name not in MEMBERS.values():
            raise ValueError(f"Unapproved path dependency: {name}")
        if name not in EDGES:
            continue
        graph[name] = set()
        for dependency in package["dependencies"]:
            dep = dependency["name"]
            if dependency.get("path"):
                dep_path = Path(dependency["path"]).resolve().relative_to(root).as_posix()
                if MEMBERS.get(dep_path) != dep:
                    raise ValueError(f"Unapproved dependency path: {name} -> {dep_path}")
            if dep in MEMBERS.values():
                graph[name].add(dep)
                if dep not in EDGES[name]:
                    raise ValueError(f"Forbidden dependency (including dev/build): {name} -> {dep}")
        if name != "fabric-cli" and any(d["name"] == "fabric-identity" for d in package["dependencies"]):
            raise ValueError("Only CLI composition may select the identity adapter")
    def visit(name: str, active: set[str], done: set[str]) -> None:
        if name in active:
            raise ValueError(f"Internal dependency cycle at {name}")
        if name in done:
            return
        for dep in graph.get(name, set()):
            visit(dep, active | {name}, done)
        done.add(name)
    done: set[str] = set()
    for name in graph:
        visit(name, set(), done)
    if metadata.get("resolve") is None:
        raise ValueError("Full resolved metadata required, not --no-deps")


def main() -> None:
    """Inspect locked metadata offline, including dev and optional dependencies."""
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--offline", "--locked", "--format-version", "1", "--all-features"],
        cwd=ROOT, text=True, timeout=120,
    ))
    validate(metadata)
    print(f"Dependency boundaries passed: {len(MEMBERS)} explicit leaves")


if __name__ == "__main__":
    main()
