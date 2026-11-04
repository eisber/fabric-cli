# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Report v2 drift from eisber/fabric-cli feature/rust; never merge or validate code.

By default all evidence is local: upstream_head_sha is the cached tracking ref,
not a claim about GitHub's current head. Explicit --fetch observes the fixed
branch and downloads that exact commit's objects, without updating ANY refs,
FETCH_HEAD, remotes, index, or worktree. Subsequent offline runs still use the
cached ref. A successful report is not code review or merge authorization.

Exit 0: a complete, safe snapshot (including normal ahead/behind divergence).
Exit 1: repository, safety, lineage, or Git/network failure, with JSON on stderr.
Exit 2: CLI syntax/usage errors. No merge, reset, checkout, commit, or push runs.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

APPROVED_BASE_SHA = "a9df45501ec53d921c6fc765c726c32979b85931"
REMOTE = "rust-upstream"
UPSTREAM_URL = "https://github.com/eisber/fabric-cli.git"
UPSTREAM_BRANCH = "refs/heads/feature/rust"
UPSTREAM_REF = "refs/remotes/rust-upstream/feature/rust"
TIMEOUT_SECONDS = 120
_SHA = re.compile(r"[0-9a-fA-F]{40}")
_OPERATIONS = (
    "MERGE_HEAD",
    "rebase-merge",
    "rebase-apply",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "sequencer",
    "BISECT_LOG",
)


class TrackingError(Exception):
    """A fail-closed tracking failure, suitable for a JSON diagnostic."""

    def __init__(
        self, code: str, message: str, details: dict[str, Any] | None = None
    ) -> None:
        """Store a stable error code without exposing Git's credential-bearing text."""
        super().__init__(message)
        self.code = code
        self.details = details or {}


def _git(
    root: Path, *arguments: str, ok_codes: tuple[int, ...] = (0,)
) -> subprocess.CompletedProcess[str]:
    environment = {
        key: value
        for key, value in os.environ.items()
        if not key.upper().startswith("GIT_")
    }
    environment.update(
        GIT_TERMINAL_PROMPT="0",
        GIT_ASKPASS="",
        GIT_OPTIONAL_LOCKS="0",
        GIT_NO_REPLACE_OBJECTS="1",
        GIT_NO_LAZY_FETCH="1",
    )
    try:
        result = subprocess.run(
            [
                "git",
                "--no-pager",
                "--no-replace-objects",
                "-c",
                "core.fsmonitor=false",
                "-c",
                f"core.hooksPath={os.devnull}",
                "-c",
                "credential.helper=",
                "-c",
                "credential.interactive=false",
                "-c",
                "core.askPass=",
                "-c",
                "http.followRedirects=false",
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.https.allow=always",
                "-c",
                "fetch.writeCommitGraph=false",
                *arguments,
            ],
            cwd=root,
            env=environment,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=TIMEOUT_SECONDS,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise TrackingError(
            "git_unavailable", f"Git {arguments[0]} could not complete."
        ) from error
    if result.returncode not in ok_codes:
        raise TrackingError(
            "git_failed",
            f"Git {arguments[0]} failed (exit {result.returncode}); no complete report.",
        )
    return result


def _commit(root: Path, value: str) -> str:
    result = _git(
        root, "rev-parse", "--verify", f"{value}^{{commit}}", ok_codes=(0, 128)
    )
    sha = result.stdout.strip()
    if result.returncode != 0 or not _SHA.fullmatch(sha):
        raise TrackingError(
            "missing_commit",
            f"Required commit/ref {value} is unavailable locally; no HEAD fallback.",
        )
    return sha.lower()


def _ancestor(root: Path, older: str, newer: str) -> bool:
    return (
        _git(root, "merge-base", "--is-ancestor", older, newer, ok_codes=(0, 1))
        .returncode
        == 0
    )


def _remote(root: Path) -> None:
    configured = _git(
        root, "config", "--get-all", f"remote.{REMOTE}.url", ok_codes=(0, 1)
    )
    if configured.returncode == 1:
        raise TrackingError("missing_remote", f"Required {REMOTE} remote is missing.")
    raw = configured.stdout.splitlines()
    expanded = _git(root, "remote", "get-url", "--all", REMOTE).stdout.splitlines()
    direct = _git(root, "ls-remote", "--get-url", UPSTREAM_URL).stdout.splitlines()
    if raw != [UPSTREAM_URL] or expanded != [UPSTREAM_URL] or direct != [UPSTREAM_URL]:
        raise TrackingError(
            "unexpected_remote",
            f"{REMOTE} must have exactly the trusted HTTPS URL, without URL rewrites.",
        )


def _worktree(root: Path) -> dict[str, Any]:
    active = []
    for operation in _OPERATIONS:
        value = _git(root, "rev-parse", "--git-path", operation).stdout.strip()
        if (root / value).exists():
            active.append(operation)
    changes = _git(
        root,
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--ignore-submodules=none",
    ).stdout
    state = {
        "clean": not changes,
        "active_operations": active,
        "porcelain": [entry for entry in changes.split("\0") if entry],
    }
    if changes or active:
        raise TrackingError(
            "unsafe_worktree",
            "Tracked/untracked changes or an active Git operation block tracking; "
            "preserve your work and finish/abort the operation manually.",
            {"worktree": state},
        )
    return state


def _fetch(root: Path) -> str:
    observed = _git(
        root, "ls-remote", "--exit-code", UPSTREAM_URL, UPSTREAM_BRANCH
    ).stdout.splitlines()
    if len(observed) != 1:
        raise TrackingError(
            "invalid_upstream", "Expected exactly one upstream branch."
        )
    fields = observed[0].split()
    if (
        len(fields) != 2
        or not _SHA.fullmatch(fields[0])
        or fields[1] != UPSTREAM_BRANCH
    ):
        raise TrackingError("invalid_upstream", "Upstream branch evidence is invalid.")
    sha = fields[0].lower()
    # No destination refspec and no FETCH_HEAD: only objects may be downloaded.
    _git(
        root,
        "fetch",
        "--no-tags",
        "--no-prune",
        "--no-prune-tags",
        "--no-recurse-submodules",
        "--no-auto-maintenance",
        "--no-write-fetch-head",
        "--refmap=",
        UPSTREAM_URL,
        sha,
    )
    if _commit(root, sha) != sha:
        raise TrackingError(
            "invalid_upstream", "Fetched upstream is not an exact commit."
        )
    return sha


def report(
    repo: Path, base_sha: str = APPROVED_BASE_SHA, *, fetch: bool = False
) -> dict[str, Any]:
    """Return exact drift and manual review guidance, or reject unsafe evidence.

    The base must be an explicit, reviewed full commit SHA present locally and
    ancestral to both HEAD and the cached upstream. With fetch, the observed
    upstream must also descend from the cached upstream (no rewind/rewrite).
    Checks trust the cached tip; they cannot detect rewrites since an unrecorded
    observation or a post-base rewrite of the cache itself.
    """
    if not _SHA.fullmatch(base_sha):
        raise TrackingError(
            "invalid_pin", "--base-sha requires a full 40-hex commit SHA."
        )
    try:
        root = repo.resolve()
    except (OSError, ValueError) as error:
        raise TrackingError(
            "invalid_repository", "--repo could not be resolved."
        ) from error
    top = Path(_git(root, "rev-parse", "--show-toplevel").stdout.strip()).resolve()
    if top != root:
        raise TrackingError("invalid_repository", "--repo must be the worktree root.")
    if _git(root, "rev-parse", "--is-shallow-repository").stdout.strip() != "false":
        raise TrackingError(
            "incomplete_history", "Shallow history cannot prove drift."
        )
    partial = _git(
        root,
        "config",
        "--get-regexp",
        r"^(extensions\.partialclone|remote\..*\.promisor)$",
        ok_codes=(0, 1),
    )
    grafts = _git(root, "rev-parse", "--git-path", "info/grafts").stdout.strip()
    if partial.returncode == 0 or (root / grafts).exists():
        raise TrackingError(
            "incomplete_history", "Partial-clone or grafted history is not supported."
        )
    _remote(root)
    base = _commit(root, base_sha)
    if base != base_sha.lower():
        raise TrackingError(
            "invalid_pin", "The base pin must name a commit, not a tag."
        )
    head = _commit(root, "HEAD")
    cached = _commit(root, UPSTREAM_REF)
    branch = _git(
        root, "symbolic-ref", "--quiet", "HEAD", ok_codes=(0, 1)
    ).stdout.strip()
    if not _ancestor(root, base, head):
        raise TrackingError(
            "head_lineage", "HEAD does not descend from the approved base."
        )
    if not _ancestor(root, base, cached):
        raise TrackingError(
            "upstream_rewritten",
            "Cached upstream does not descend from the approved base.",
        )
    _worktree(root)
    upstream = _fetch(root) if fetch else cached
    if not _ancestor(root, cached, upstream):
        raise TrackingError(
            "upstream_rewritten",
            "Observed upstream rewound or rewrote cached ancestry; "
            "manual investigation required.",
        )
    counts = _git(
        root, "rev-list", "--left-right", "--count", f"{head}...{upstream}"
    ).stdout.split()
    if len(counts) != 2 or not all(value.isdecimal() for value in counts):
        raise TrackingError(
            "invalid_counts", "Git did not return exact ahead/behind counts."
        )
    safety = _worktree(root)
    _remote(root)
    if (
        _commit(root, "HEAD") != head
        or _commit(root, UPSTREAM_REF) != cached
        or _git(
            root, "symbolic-ref", "--quiet", "HEAD", ok_codes=(0, 1)
        ).stdout.strip()
        != branch
    ):
        raise TrackingError(
            "repository_changed", "Repository changed during observation."
        )
    return {
        "ok": True,
        "base_sha": base,
        "head_sha": head,
        "branch": branch or None,
        "upstream_head_sha": upstream,
        "cached_upstream_sha": cached,
        "upstream": {
            "url": UPSTREAM_URL,
            "branch": UPSTREAM_BRANCH,
            "tracking_ref": UPSTREAM_REF,
            "source": "fetched_branch_snapshot" if fetch else "local_tracking_ref",
            "freshness": (
                "Observed at ls-remote; the remote may advance again."
                if fetch
                else "Cached only; current GitHub head is unknown. "
                "Use --fetch to observe it."
            ),
            "tracking_ref_updated": False,
        },
        "ahead": int(counts[0]),
        "behind": int(counts[1]),
        "lineage": {
            "base_is_ancestor_of_head": True,
            "base_is_ancestor_of_upstream": True,
            "cached_is_ancestor_of_observed": True if fetch else None,
            "limits": "Checks trust base/cached ancestry, not unrecorded observations "
            "or a post-base rewrite of the cache itself.",
        },
        "worktree": safety,
        "manual_integration": {
            "executed": False,
            "expected_head_sha": head,
            "expected_branch": branch or None,
            "review_commands": [
                ["git", "log", "--oneline", f"{head}..{upstream}"],
                ["git", "diff", f"{head}...{upstream}"],
            ],
            "merge_command": ["git", "merge", "--no-ff", "--no-commit", upstream],
            "instructions": [
                "This report neither reviews nor validates code, "
                "and grants no merge authority.",
                "Review this exact upstream SHA and obtain "
                "integration-owner authorization.",
                "Use only your authorized v2 branch, never feature/rust "
                "or a peer worktree.",
                "Before merging, recheck branch/HEAD against this snapshot "
                "and require a clean worktree with no active Git operation. "
                "A detached HEAD must first be placed "
                "on an authorized v2 branch; rerun the report afterward.",
                "Run the merge command manually only after review. It names the exact SHA, "
                "not a moving ref; resolve conflicts, review and validate the combined "
                "candidate before committing. Do not reset, force-push, or auto-merge.",
            ],
        },
    }


def main(argv: list[str] | None = None) -> int:
    """Print a JSON drift report or a non-success JSON error on stderr."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo", type=Path, default=Path(__file__).resolve().parents[2]
    )
    parser.add_argument(
        "--base-sha",
        default=APPROVED_BASE_SHA,
        help=f"Reviewed full commit SHA (default: {APPROVED_BASE_SHA}).",
    )
    parser.add_argument(
        "--fetch",
        action="store_true",
        help="Explicit network opt-in: observe only trusted feature/rust and fetch its "
        "exact objects; do not update tracking refs or FETCH_HEAD.",
    )
    arguments = parser.parse_args(argv)
    try:
        result = report(arguments.repo, arguments.base_sha, fetch=arguments.fetch)
    except TrackingError as error:
        print(
            json.dumps(
                {
                    "ok": False,
                    "error": {"code": error.code, "message": str(error)},
                    **error.details,
                },
                sort_keys=True,
            ),
            file=sys.stderr,
        )
        return 1
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
