# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Small offline guards; these do not run the full-scalar sweep."""

import contextlib
import io
import sys
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

import generate


class UnicodeGeneratorTests(unittest.TestCase):
    """Keep provenance, bounded execution and independent source behavior explicit."""

    def test_exact_runtime_versions_are_mandatory(self) -> None:
        with patch.object(generate.platform, "python_version", return_value="3.14.0"):
            with self.assertRaisesRegex(AssertionError, "requires CPython"):
                generate.pinned_runtime()

    def test_source_scalar_width_is_not_rich_width(self) -> None:
        cells = generate.pinned_runtime()
        namespace = generate.source_namespace()
        self.assertEqual(namespace["_get_visual_length"]("e\u0301"), 2)
        self.assertEqual(cells.cell_len("e\u0301", generate.RICH_UCD), 1)
        self.assertEqual(namespace["_char_visual_width"]("\0"), 1)
        self.assertEqual(cells.get_character_cell_size("\0", generate.RICH_UCD), 0)
        self.assertEqual(generate.scalar_record(0x1FAE9, namespace, cells)[2], 0)

    def test_sigma_probe_handles_cased_ignorables(self) -> None:
        for character in ("\u0345", "\u0301", "'", "\u200d", "\u00ad"):
            with self.subTest(point=ord(character)):
                self.assertTrue(generate.is_case_ignorable(character))
        for character in ("A", "a", " ", "0", "\0", "\u03a3"):
            with self.subTest(point=ord(character)):
                self.assertFalse(generate.is_case_ignorable(character))

    def test_whole_oracle_executes_original_key_and_width_functions(self) -> None:
        oracle = generate.whole_oracle(
            generate.source_namespace(), generate.pinned_runtime()
        )
        self.assertEqual(len(oracle["texts"]), 48)
        self.assertEqual(len(oracle["keys"]), 31)
        self.assertEqual(len(oracle["values"]), 62)
        self.assertEqual(len(oracle["tables"]), 40)
        self.assertEqual(len(oracle["render_cases"]), 3)
        by_key = {case["key"]: case for case in oracle["keys"]}
        self.assertEqual(by_key["\u00df_id"]["expected"], "Ss ID")
        self.assertEqual(by_key["\u03bf\u03a3_id"]["expected"], "\u039f\u03c2 ID")
        self.assertIn("error", by_key["camelCase"])
        self.assertEqual(oracle["source_only_domain"]["surrogate_source_width"], 1)

    def test_installed_rich_code_and_data_have_separate_hashes(self) -> None:
        hashes = generate.rich_source_hashes(generate.pinned_runtime())
        self.assertEqual(
            set(hashes),
            {"rich.cells", "rich._unicode_data", "rich._unicode_data.unicode17-0-0"},
        )
        self.assertTrue(all(len(value) == 64 for value in hashes.values()))

    def test_full_sweep_requires_explicit_controller_slot(self) -> None:
        with (
            patch.object(sys, "argv", ["generate.py", "--generate"]),
            patch.object(generate, "build", side_effect=AssertionError("sweep ran")),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            with self.assertRaises(SystemExit) as failure:
                generate.main()
            self.assertEqual(failure.exception.code, 2)

    def test_existing_artifacts_cannot_be_overwritten(self) -> None:
        with (
            patch.object(
                sys, "argv", ["generate.py", "--generate", "--full-scalar-slot"]
            ),
            patch.object(Path, "exists", return_value=True),
            patch.object(generate, "build", side_effect=AssertionError("sweep ran")),
        ):
            with self.assertRaisesRegex(AssertionError, "refuses existing"):
                generate.main()

    def test_default_command_only_runs_smoke(self) -> None:
        with (
            patch.object(sys, "argv", ["generate.py"]),
            patch.object(generate, "smoke") as smoke,
            patch.object(generate, "build", side_effect=AssertionError("sweep ran")),
        ):
            generate.main()
            smoke.assert_called_once()

    def test_interrupted_generation_removes_only_its_new_artifacts(self) -> None:
        original_open = Path.open
        opened: list[Path] = []
        removed: list[Path] = []

        def selective_open(
            path: Path, mode: str = "r", *args: Any, **kwargs: Any
        ) -> Any:
            if mode != "xb":
                return original_open(path, mode, *args, **kwargs)
            opened.append(path)
            if len(opened) == 2:
                raise OSError("controlled artifact write failure")
            return io.BytesIO()

        def record_remove(path: Path, *, missing_ok: bool = False) -> None:
            self.assertTrue(missing_ok)
            removed.append(path)

        with (
            patch.object(
                sys, "argv", ["generate.py", "--generate", "--full-scalar-slot"]
            ),
            patch.object(generate, "pinned_runtime", return_value=object()),
            patch.object(generate, "source_namespace", return_value={}),
            patch.object(generate, "build", return_value=({}, {})),
            patch.object(Path, "exists", return_value=False),
            patch.object(Path, "open", new=selective_open),
            patch.object(Path, "unlink", new=record_remove),
        ):
            with self.assertRaisesRegex(OSError, "controlled artifact"):
                generate.main()
        self.assertEqual(removed, [generate.HERE / "source-data.json"])

    def test_verification_rejects_noncanonical_artifact_bytes(self) -> None:
        original_read = Path.read_bytes

        def selective_read(path: Path) -> bytes:
            if path == generate.HERE / "source-data.json":
                return b"{} \n"
            return original_read(path)

        with (
            patch.object(
                sys, "argv", ["generate.py", "--verify", "--full-scalar-slot"]
            ),
            patch.object(generate, "pinned_runtime", return_value=object()),
            patch.object(generate, "source_namespace", return_value={}),
            patch.object(generate, "build", return_value=({}, {})),
            patch.object(Path, "exists", return_value=True),
            patch.object(Path, "read_bytes", new=selective_read),
        ):
            with self.assertRaisesRegex(AssertionError, "source-data.json"):
                generate.main()


if __name__ == "__main__":
    unittest.main()
