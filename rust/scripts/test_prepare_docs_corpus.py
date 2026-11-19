# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Offline acquisition contracts; no provider access or Git execution."""

import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import prepare_docs_corpus as corpus


class PrepareCorpusTests(unittest.TestCase):
    """Verify acquisition scope and failure contracts without network access."""

    def test_sparse_acquisition_pins_revision_and_preserves_notices(self) -> None:
        """Only pinned text paths and explicit notices are materialized."""
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "corpus"
            commands = []

            def execute(args: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
                commands.append(args)
                self.assertTrue(kwargs["check"])
                self.assertEqual(kwargs["timeout"], 600)
                if "clone" in args:
                    (output / "docs").mkdir(parents=True)
                    (output / "docs" / "topic.md").write_text("# Topic\n", encoding="utf-8")
                    (output / "LICENSE").write_text("Content notice", encoding="utf-8")
                    (output / "LICENSE-CODE").write_text("Code notice", encoding="utf-8")
                stdout = corpus.PINNED_REVISION if "rev-parse" in args else ""
                return subprocess.CompletedProcess(args, 0, stdout, "")

            with patch.object(corpus.subprocess, "run", side_effect=execute):
                result = corpus.prepare(output, corpus.PINNED_REVISION)
            self.assertEqual(result["commit"], corpus.PINNED_REVISION)
            self.assertEqual(result["markdown_files"], 1)
            self.assertIn("--filter=blob:none", commands[0])
            self.assertIn("core.autocrlf=false", commands[0])
            sparse = next(args for args in commands if "sparse-checkout" in args)
            self.assertIn("/LICENSE-CODE", sparse)
            self.assertFalse(any("*.png" in args for args in commands))
            self.assertIn("checkout", commands[-2])

    def test_existing_or_relative_output_and_invalid_revision_do_not_run_git(self) -> None:
        """Invalid requests cannot overwrite a path or launch Git."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            existing = root / "existing"
            existing.mkdir()
            sentinel = existing / "preserve"
            sentinel.write_bytes(b"untouched")
            with patch.object(corpus.subprocess, "run") as run:
                for output, revision in [
                    (existing, corpus.PINNED_REVISION),
                    (Path("relative"), corpus.PINNED_REVISION),
                    (root / "new", "main"),
                    (root / "new", "A" * 40),
                ]:
                    with self.assertRaises(ValueError):
                        corpus.prepare(output, revision)
                run.assert_not_called()
            self.assertEqual(sentinel.read_bytes(), b"untouched")

    def test_git_failure_is_not_an_acquisition_receipt(self) -> None:
        """Acquisition failures propagate rather than yielding invented evidence."""
        with tempfile.TemporaryDirectory() as temporary:
            failure = subprocess.CalledProcessError(128, ["git"], stderr="source unavailable")
            with patch.object(corpus.subprocess, "run", side_effect=failure):
                with self.assertRaises(subprocess.CalledProcessError):
                    corpus.prepare(Path(temporary) / "new", corpus.PINNED_REVISION)


if __name__ == "__main__":
    unittest.main()
