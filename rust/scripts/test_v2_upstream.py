# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Offline synthetic-Git tests for the developer-only v2 tracking helper."""

from __future__ import annotations

import contextlib
import io
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import Any, Callable
from unittest.mock import patch

import v2_upstream as tracker


class UpstreamTests(unittest.TestCase):
    """Exercise exact commit graphs without contacting any real remote."""

    def setUp(self) -> None:
        """Create a uniquely owned transient synthetic repository."""
        self.directory = tempfile.TemporaryDirectory(prefix="v2-upstream-tests-")
        self.root = Path(self.directory.name).resolve()
        self.addCleanup(self.cleanup)
        self.git("init", "--quiet", "--initial-branch=v2-test")
        self.write("tracked.txt", "base\n")
        self.git("add", "tracked.txt")
        self.tree = self.git("write-tree").stdout.strip()
        self.base = self.commit("approved base")
        self.git("update-ref", "HEAD", self.base)
        self.git("remote", "add", tracker.REMOTE, tracker.UPSTREAM_URL)
        self.git("update-ref", tracker.UPSTREAM_REF, self.base)
        self.real_git = tracker._git
        self.calls: list[tuple[str, ...]] = []
        guard = patch.object(tracker, "_git", side_effect=self.offline_git)
        guard.start()
        self.addCleanup(guard.stop)

    def cleanup(self) -> None:
        """Remove only this test's uniquely generated fixture, not its parent."""
        for path in self.root.rglob("*"):
            if path.is_file():
                path.chmod(0o600)
        self.directory.cleanup()

    def git(
        self, *arguments: str, data: str | None = None
    ) -> subprocess.CompletedProcess[str]:
        """Run fixture plumbing with no global identity, hooks, or signing."""
        environment = {
            key: value
            for key, value in os.environ.items()
            if not key.upper().startswith("GIT_")
        }
        environment.update(
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_AUTHOR_NAME="Offline test",
            GIT_AUTHOR_EMAIL="offline@example.invalid",
            GIT_COMMITTER_NAME="Offline test",
            GIT_COMMITTER_EMAIL="offline@example.invalid",
        )
        return subprocess.run(
            [
                "git",
                "--no-pager",
                "-c",
                "core.autocrlf=false",
                "-c",
                f"core.hooksPath={os.devnull}",
                "-c",
                "commit.gpgsign=false",
                *arguments,
            ],
            cwd=self.root,
            env=environment,
            input=data,
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=True,
            timeout=30,
        )

    def write(self, name: str, value: str) -> None:
        """Write only a file inside this synthetic worktree."""
        (self.root / name).write_text(value, encoding="utf-8")

    def commit(self, message: str, *parents: str) -> str:
        """Create an object with exact parents without moving any branch."""
        arguments = [value for parent in parents for value in ("-p", parent)]
        return self.git(
            "commit-tree", self.tree, *arguments, data=message
        ).stdout.strip()

    def offline_git(
        self, root: Path, *arguments: str, **kwargs: Any
    ) -> subprocess.CompletedProcess[str]:
        """Fail the test before any accidental network or mutating Git command."""
        self.calls.append(arguments)
        self.assertIn(
            arguments[0],
            {
                "rev-parse",
                "config",
                "remote",
                "ls-remote",
                "symbolic-ref",
                "merge-base",
                "status",
                "rev-list",
            },
        )
        if arguments[0] == "ls-remote":
            self.assertEqual(
                arguments, ("ls-remote", "--get-url", tracker.UPSTREAM_URL)
            )
        if arguments[0] == "remote":
            self.assertEqual(arguments, ("remote", "get-url", "--all", tracker.REMOTE))
        if arguments[0] == "config":
            self.assertIn(arguments[1], ("--get-all", "--get-regexp"))
        with patch.dict(
            os.environ,
            HOME=str(self.root),
            USERPROFILE=str(self.root),
            XDG_CONFIG_HOME=str(self.root),
        ):
            return self.real_git(root, *arguments, **kwargs)

    def report(self, *, fetch: bool = False) -> dict[str, Any]:
        """Run against the synthetic approved checkpoint."""
        return tracker.report(self.root, self.base, fetch=fetch)

    def snapshot(self) -> dict[str, bytes]:
        """Capture repository bytes, including refs, logs, index and worktree."""
        return {
            path.relative_to(self.root).as_posix(): path.read_bytes()
            for path in self.root.rglob("*")
            if path.is_file()
        }

    def test_default_is_offline_and_byte_for_byte_read_only(self) -> None:
        """A default report neither contacts GitHub nor changes any local bytes."""
        before = self.snapshot()
        result = self.report()
        self.assertEqual(before, self.snapshot())
        self.assertEqual(result["head_sha"], self.base)
        self.assertEqual(result["upstream_head_sha"], self.base)
        self.assertEqual((result["ahead"], result["behind"]), (0, 0))
        self.assertEqual(result["upstream"]["source"], "local_tracking_ref")
        self.assertIn("unknown", result["upstream"]["freshness"])
        self.assertIsNone(result["lineage"]["cached_is_ancestor_of_observed"])
        self.assertEqual(result["worktree"]["active_operations"], [])
        self.assertTrue(result["worktree"]["clean"])
        self.assertFalse(result["manual_integration"]["executed"])

    def test_genuinely_advanced_upstream_uses_actual_head_not_base(self) -> None:
        """Count all commits on an upstream chain rather than comparing to base."""
        first = self.commit("upstream one", self.base)
        second = self.commit("upstream two", first)
        third = self.commit("upstream three", second)
        self.git("update-ref", tracker.UPSTREAM_REF, third)
        result = self.report()
        self.assertEqual(result["upstream_head_sha"], third)
        self.assertEqual((result["ahead"], result["behind"]), (0, 3))
        self.assertEqual(
            result["manual_integration"]["merge_command"],
            ["git", "merge", "--no-ff", "--no-commit", third],
        )
        self.assertIn(
            f"{self.base}...{third}",
            result["manual_integration"]["review_commands"][1],
        )

    def test_expected_v2_divergence_is_not_an_upstream_rewrite(self) -> None:
        """Local and upstream descendants of the pin can normally diverge."""
        local = self.commit("v2 one", self.base)
        local = self.commit("v2 two", local)
        upstream = self.commit("upstream one", self.base)
        upstream = self.commit("upstream two", upstream)
        upstream = self.commit("upstream three", upstream)
        self.git("update-ref", "HEAD", local)
        self.git("update-ref", tracker.UPSTREAM_REF, upstream)
        result = self.report()
        self.assertEqual((result["ahead"], result["behind"]), (2, 3))
        self.assertEqual(result["head_sha"], local)
        self.assertTrue(result["lineage"]["base_is_ancestor_of_upstream"])

    def test_ahead_head_and_already_integrated_upstream(self) -> None:
        """Count only local commits once HEAD has incorporated upstream."""
        upstream = self.commit("upstream", self.base)
        local = self.commit("v2", upstream)
        self.git("update-ref", tracker.UPSTREAM_REF, upstream)
        self.git("update-ref", "HEAD", local)
        self.assertEqual((self.report()["ahead"], self.report()["behind"]), (1, 0))

    def test_rewritten_cached_upstream_is_rejected(self) -> None:
        """An unrelated upstream cannot silently become the new base."""
        rewritten = self.commit("unrelated upstream")
        self.git("update-ref", tracker.UPSTREAM_REF, rewritten)
        with self.assertRaisesRegex(tracker.TrackingError, "Cached upstream"):
            self.report()

    def test_unrelated_head_is_rejected(self) -> None:
        """A checkout from a different lineage is not a valid v2 candidate."""
        self.git("update-ref", "HEAD", self.commit("unrelated head"))
        with self.assertRaisesRegex(tracker.TrackingError, "HEAD does not descend"):
            self.report()

    def test_invalid_missing_and_non_commit_pins_are_rejected(self) -> None:
        """Pins must be full, available commit identities, never moving refs."""
        for value in ("HEAD", self.base[:12], "-" + "1" * 39, "g" * 40, "1" * 40):
            with self.subTest(value=value):
                with self.assertRaises(tracker.TrackingError):
                    tracker.report(self.root, value)
        with self.assertRaises(tracker.TrackingError):
            tracker.report(self.root, self.tree)
        self.git("tag", "--annotate", "--message=fixture", "fixture", self.base)
        tag = self.git("rev-parse", "refs/tags/fixture").stdout.strip()
        with self.assertRaisesRegex(tracker.TrackingError, "not a tag"):
            tracker.report(self.root, tag)

    def test_missing_ref_and_missing_remote_fail_without_head_fallback(self) -> None:
        """Do not claim zero drift when required local evidence is absent."""
        self.git("update-ref", "-d", tracker.UPSTREAM_REF)
        with self.assertRaisesRegex(tracker.TrackingError, "unavailable locally"):
            self.report()
        self.git("update-ref", tracker.UPSTREAM_REF, self.base)
        self.git("remote", "remove", tracker.REMOTE)
        with self.assertRaises(tracker.TrackingError):
            self.report()

    def test_unexpected_multiple_and_rewritten_remote_urls_are_rejected(self) -> None:
        """Reject alternate transport, extra URLs, or effective URL rewrites."""
        for url in (
            "https://github.com/microsoft/fabric-cli.git",
            "git@github.com:eisber/fabric-cli.git",
            "https://github.com/eisber/fabric-cli.git/extra",
        ):
            with self.subTest(url=url):
                self.git("remote", "set-url", tracker.REMOTE, url)
                with self.assertRaisesRegex(tracker.TrackingError, "trusted HTTPS"):
                    self.report(fetch=True)
        self.git("remote", "set-url", tracker.REMOTE, tracker.UPSTREAM_URL)
        self.git(
            "config", "--add", f"remote.{tracker.REMOTE}.url", tracker.UPSTREAM_URL
        )
        with self.assertRaisesRegex(tracker.TrackingError, "trusted HTTPS"):
            self.report()
        self.git("config", "--unset-all", f"remote.{tracker.REMOTE}.url")
        self.git("config", f"remote.{tracker.REMOTE}.url", tracker.UPSTREAM_URL)
        self.git(
            "config", "url.https://example.invalid/.insteadOf", tracker.UPSTREAM_URL
        )
        with self.assertRaisesRegex(tracker.TrackingError, "trusted HTTPS"):
            self.report(fetch=True)

    def test_dirty_tracked_staged_and_untracked_files_are_rejected(self) -> None:
        """A tracked edit, index edit or new file blocks even an explicit fetch."""
        self.write("tracked.txt", "dirty\n")
        with self.assertRaises(tracker.TrackingError) as caught:
            self.report(fetch=True)
        self.assertFalse(caught.exception.details["worktree"]["clean"])
        self.git("add", "tracked.txt")
        with self.assertRaises(tracker.TrackingError):
            self.report()
        self.write("tracked.txt", "base\n")
        self.git("add", "tracked.txt")
        self.write("untracked.txt", "preserve this work\n")
        with self.assertRaises(tracker.TrackingError) as caught:
            self.report(fetch=True)
        self.assertIn(
            "?? untracked.txt", caught.exception.details["worktree"]["porcelain"]
        )
        self.assertEqual(
            (self.root / "untracked.txt").read_text(), "preserve this work\n"
        )

    def test_active_operations_are_rejected_even_when_status_is_clean(self) -> None:
        """In-progress Git operations are not safe merge starting points."""
        for name in tracker._OPERATIONS:
            marker = self.root / ".git" / name
            marker.write_text(self.base + "\n", encoding="utf-8")
            try:
                with self.subTest(operation=name):
                    with self.assertRaises(tracker.TrackingError) as caught:
                        self.report(fetch=True)
                    self.assertIn(
                        name, caught.exception.details["worktree"]["active_operations"]
                    )
            finally:
                marker.unlink()

    def test_shallow_partial_and_grafted_repositories_are_rejected(self) -> None:
        """Do not invent complete counts from truncated or altered history."""
        shallow = self.root / ".git" / "shallow"
        shallow.write_text(self.base + "\n", encoding="utf-8")
        with self.assertRaisesRegex(tracker.TrackingError, "Shallow"):
            self.report()
        shallow.unlink()
        self.git("config", f"remote.{tracker.REMOTE}.promisor", "true")
        with self.assertRaisesRegex(tracker.TrackingError, "Partial-clone"):
            self.report()
        self.git("config", "--unset", f"remote.{tracker.REMOTE}.promisor")
        (self.root / ".git" / "info" / "grafts").write_text("", encoding="utf-8")
        with self.assertRaisesRegex(tracker.TrackingError, "grafted"):
            self.report()

    def fetched_git(
        self, observed: str, downloads: list[tuple[str, ...]]
    ) -> Callable[..., subprocess.CompletedProcess[str]]:
        """Permit only fixed-origin, object-only fetches in the network stub."""

        def run(
            root: Path, *arguments: str, **kwargs: Any
        ) -> subprocess.CompletedProcess[str]:
            if arguments == (
                "ls-remote",
                "--exit-code",
                tracker.UPSTREAM_URL,
                tracker.UPSTREAM_BRANCH,
            ):
                downloads.append(arguments)
                return subprocess.CompletedProcess(
                    arguments, 0, f"{observed}\t{tracker.UPSTREAM_BRANCH}\n", ""
                )
            if "fetch" in arguments:
                downloads.append(arguments)
                self.assertEqual(
                    arguments,
                    (
                        "fetch",
                        "--no-tags",
                        "--no-prune",
                        "--no-prune-tags",
                        "--no-recurse-submodules",
                        "--no-auto-maintenance",
                        "--no-write-fetch-head",
                        "--refmap=",
                        tracker.UPSTREAM_URL,
                        observed,
                    ),
                )
                return subprocess.CompletedProcess(arguments, 0, "", "")
            return self.offline_git(root, *arguments, **kwargs)

        return run

    def test_explicit_fetch_observes_exact_branch_and_never_updates_refs(self) -> None:
        """Explicit opt-in observes advancement without changing FETCH_HEAD."""
        advanced = self.commit("new upstream", self.base)
        (self.root / ".git" / "FETCH_HEAD").write_text("retain\n", encoding="utf-8")
        downloads: list[tuple[str, ...]] = []
        before = self.snapshot()
        with patch.object(
            tracker, "_git", side_effect=self.fetched_git(advanced, downloads)
        ):
            result = self.report(fetch=True)
        self.assertEqual(len(downloads), 2)
        self.assertEqual(result["upstream_head_sha"], advanced)
        self.assertEqual(result["cached_upstream_sha"], self.base)
        self.assertEqual((result["ahead"], result["behind"]), (0, 1))
        self.assertEqual(result["upstream"]["source"], "fetched_branch_snapshot")
        self.assertFalse(result["upstream"]["tracking_ref_updated"])
        self.assertEqual(before, self.snapshot())
        self.assertEqual(self.report()["upstream_head_sha"], self.base)

    def test_real_local_fetch_changes_only_object_storage(self) -> None:
        """Exercise actual Git fetch flags against a tiny local bare upstream."""
        source = self.root / ".git" / "offline-source.git"
        self.git("clone", "--bare", "--no-hardlinks", str(self.root), str(source))
        advanced = self.git(
            "--git-dir",
            str(source),
            "commit-tree",
            self.tree,
            "-p",
            self.base,
            data="only available in the synthetic upstream",
        ).stdout.strip()
        self.git(
            "--git-dir", str(source), "update-ref", tracker.UPSTREAM_BRANCH, advanced
        )
        with self.assertRaises(subprocess.CalledProcessError):
            self.git("cat-file", "-e", advanced)
        self.git("tag", "retained-local-tag", self.base)
        self.git("config", "fetch.prune", "true")
        self.git("config", "fetch.pruneTags", "true")
        self.git("config", "fetch.writeCommitGraph", "true")
        (self.root / ".git" / "FETCH_HEAD").write_text("retain\n", encoding="utf-8")
        before = self.snapshot()

        def local_transport(
            root: Path, *arguments: str, **kwargs: Any
        ) -> subprocess.CompletedProcess[str]:
            if arguments[:2] == ("ls-remote", "--exit-code"):
                self.assertEqual(
                    arguments,
                    (
                        "ls-remote",
                        "--exit-code",
                        tracker.UPSTREAM_URL,
                        tracker.UPSTREAM_BRANCH,
                    ),
                )
                return self.git(
                    "ls-remote", "--exit-code", str(source), tracker.UPSTREAM_BRANCH
                )
            if "fetch" in arguments:
                # Replace only the validated trusted origin with this owned fixture.
                self.assertEqual(arguments[-2:], (tracker.UPSTREAM_URL, advanced))
                return self.git(*arguments[:-2], str(source), advanced)
            return self.offline_git(root, *arguments, **kwargs)

        with patch.object(tracker, "_git", side_effect=local_transport):
            result = self.report(fetch=True)
        self.assertEqual(result["upstream_head_sha"], advanced)
        self.assertEqual(result["behind"], 1)
        self.git("cat-file", "-e", advanced)
        after = self.snapshot()
        self.assertEqual(before, {path: after[path] for path in before})
        added = after.keys() - before.keys()
        self.assertTrue(added)
        self.assertTrue(all(path.startswith(".git/objects/") for path in added))

    def test_linked_worktree_uses_its_own_operation_markers(self) -> None:
        """Resolve per-worktree Git paths rather than assuming a .git directory."""
        linked = self.root / ".git" / "linked-checkout"
        self.git(
            "worktree", "add", "--quiet", "-b", "v2-linked", str(linked), self.base
        )
        self.assertTrue((linked / ".git").is_file())
        before = self.snapshot()
        self.assertTrue(tracker.report(linked, self.base)["worktree"]["clean"])
        self.assertEqual(before, self.snapshot())
        merge = Path(
            self.git(
                "-C", str(linked), "rev-parse", "--git-path", "MERGE_HEAD"
            ).stdout.strip()
        )
        merge.write_text(self.base + "\n", encoding="utf-8")
        with self.assertRaises(tracker.TrackingError) as caught:
            tracker.report(linked, self.base)
        self.assertIn(
            "MERGE_HEAD", caught.exception.details["worktree"]["active_operations"]
        )
        self.assertTrue(self.report()["worktree"]["clean"])

    def test_fetched_rewind_and_post_base_rewrite_are_rejected(self) -> None:
        """Even descendants of the base must retain the previously cached lineage."""
        cached = self.commit("cached upstream", self.base)
        self.git("update-ref", tracker.UPSTREAM_REF, cached)
        for observed in (self.base, self.commit("rewritten descendant", self.base)):
            downloads: list[tuple[str, ...]] = []
            with self.subTest(observed=observed):
                with patch.object(
                    tracker, "_git", side_effect=self.fetched_git(observed, downloads)
                ):
                    with self.assertRaisesRegex(
                        tracker.TrackingError, "rewound or rewrote"
                    ):
                        self.report(fetch=True)
            self.assertEqual(
                self.git("rev-parse", tracker.UPSTREAM_REF).stdout.strip(), cached
            )

    def test_network_failure_and_malformed_observation_fail_closed(self) -> None:
        """No cached-success fallback after explicitly requested refresh fails."""
        for response in (
            "",
            "not-a-sha\trefs/heads/feature/rust\n",
            f"{self.base}\tHEAD\n",
        ):

            def malformed(
                root: Path, *arguments: str, **kwargs: Any
            ) -> subprocess.CompletedProcess[str]:
                if arguments[:2] == ("ls-remote", "--exit-code"):
                    return subprocess.CompletedProcess(arguments, 0, response, "")
                return self.offline_git(root, *arguments, **kwargs)

            with patch.object(tracker, "_git", side_effect=malformed):
                with self.assertRaises(tracker.TrackingError):
                    self.report(fetch=True)

        def failed(
            root: Path, *arguments: str, **kwargs: Any
        ) -> subprocess.CompletedProcess[str]:
            if arguments[:2] == ("ls-remote", "--exit-code"):
                raise tracker.TrackingError("git_failed", "Git ls-remote failed.")
            return self.offline_git(root, *arguments, **kwargs)

        with patch.object(tracker, "_git", side_effect=failed):
            with self.assertRaisesRegex(tracker.TrackingError, "ls-remote failed"):
                self.report(fetch=True)

    def test_download_failure_has_no_cached_success_fallback(self) -> None:
        """A successful observation cannot conceal an unsuccessful object transfer."""
        downloads: list[tuple[str, ...]] = []
        stub = self.fetched_git(self.base, downloads)

        def failed_fetch(
            root: Path, *arguments: str, **kwargs: Any
        ) -> subprocess.CompletedProcess[str]:
            if arguments[0] == "fetch":
                raise tracker.TrackingError("git_failed", "Git fetch failed.")
            return stub(root, *arguments, **kwargs)

        with patch.object(tracker, "_git", side_effect=failed_fetch):
            with self.assertRaisesRegex(tracker.TrackingError, "Git fetch failed"):
                self.report(fetch=True)

    def test_changed_head_and_failed_counting_cannot_yield_success(self) -> None:
        """Reject failed counting and concurrent ref changes instead of guessing."""
        advanced = self.commit("concurrent v2", self.base)

        def changed(
            root: Path, *arguments: str, **kwargs: Any
        ) -> subprocess.CompletedProcess[str]:
            if arguments[0] == "rev-list":
                self.git("update-ref", "HEAD", advanced)
            return self.offline_git(root, *arguments, **kwargs)

        with patch.object(tracker, "_git", side_effect=changed):
            with self.assertRaisesRegex(tracker.TrackingError, "changed during"):
                self.report()

        def bad_counts(
            root: Path, *arguments: str, **kwargs: Any
        ) -> subprocess.CompletedProcess[str]:
            if arguments[0] == "rev-list":
                return subprocess.CompletedProcess(arguments, 0, "unknown", "")
            return self.offline_git(root, *arguments, **kwargs)

        with patch.object(tracker, "_git", side_effect=bad_counts):
            with self.assertRaisesRegex(tracker.TrackingError, "exact ahead/behind"):
                self.report()

    def test_git_wrapper_disables_ambient_mutation_and_sanitizes_errors(self) -> None:
        """Use noninteractive, replacement-free Git without ambient credentials."""
        result = subprocess.CompletedProcess([], 0, "output", "")
        with patch.dict(os.environ, {"GIT_DIR": "wrong", "GIT_TRACE": "secret-path"}):
            with patch.object(tracker.subprocess, "run", return_value=result) as run:
                self.real_git(self.root, "status", "--porcelain")
        environment = run.call_args.kwargs["env"]
        self.assertNotIn("GIT_DIR", environment)
        self.assertNotIn("GIT_TRACE", environment)
        self.assertEqual(environment["GIT_OPTIONAL_LOCKS"], "0")
        self.assertEqual(environment["GIT_NO_LAZY_FETCH"], "1")
        self.assertEqual(environment["GIT_NO_REPLACE_OBJECTS"], "1")
        self.assertIn("credential.helper=", run.call_args.args[0])
        with patch.object(
            tracker.subprocess,
            "run",
            return_value=subprocess.CompletedProcess([], 9, "", "secret"),
        ):
            with self.assertRaises(tracker.TrackingError) as caught:
                self.real_git(self.root, "rev-list")
        self.assertNotIn("secret", str(caught.exception))
        for failure in (OSError("secret"), subprocess.TimeoutExpired("git", 1)):
            with patch.object(tracker.subprocess, "run", side_effect=failure):
                with self.assertRaises(tracker.TrackingError):
                    self.real_git(self.root, "rev-list")

    def test_cli_json_success_error_and_exit_semantics(self) -> None:
        """A failed report prints no success JSON and exits nonzero."""
        output, error = io.StringIO(), io.StringIO()
        arguments = ["--repo", str(self.root), "--base-sha", self.base]
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(error):
            self.assertEqual(tracker.main(arguments), 0)
        self.assertTrue(json.loads(output.getvalue())["ok"])
        self.assertEqual(error.getvalue(), "")
        output, error = io.StringIO(), io.StringIO()
        self.write("unsafe.txt", "untracked\n")
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(error):
            self.assertEqual(tracker.main(arguments), 1)
        self.assertEqual(output.getvalue(), "")
        self.assertEqual(
            json.loads(error.getvalue())["error"]["code"], "unsafe_worktree"
        )
        with contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as caught:
                tracker.main(["--unknown"])
        self.assertEqual(caught.exception.code, 2)

    def test_cli_default_is_the_exact_approved_checkpoint(self) -> None:
        """Default pins cannot silently follow the current branch or upstream."""
        approved = "a9df45501ec53d921c6fc765c726c32979b85931"
        self.assertEqual(tracker.APPROVED_BASE_SHA, approved)
        with patch.object(tracker, "report", return_value={"ok": True}) as run:
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(tracker.main(["--repo", str(self.root)]), 0)
        run.assert_called_once_with(self.root, approved, fetch=False)


if __name__ == "__main__":
    unittest.main()
