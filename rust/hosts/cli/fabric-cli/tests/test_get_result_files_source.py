# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Observe GET output storage directly from immutable cf67 source, offline."""

from __future__ import annotations

import io
import json
import os
from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from typing import Any
import unittest

from test_get_options_source import load


class ResultFilesSource(unittest.TestCase):
    """Source names, output ordering, serialization and overwrite policy."""

    def setUp(self) -> None:
        self.events: list[Any] = []
        self.content: dict[str, str] = {}
        content = self.content
        events = self.events

        class Output(io.StringIO):
            def __init__(self, path: str) -> None:
                super().__init__()
                self.path = path

            def close(self) -> None:
                content[self.path] = self.getvalue()
                super().close()

        def open_output(path: str, mode: str, **kwargs: Any) -> Output:
            self.events.append(("open", path, mode, kwargs))
            return Output(path)

        clock = SimpleNamespace(
            now=lambda: SimpleNamespace(
                strftime=lambda pattern: events.append(("clock", pattern))
                or "20261102_135959"
            )
        )
        self.storage = load(
            "utils/fab_storage.py",
            {
                "do_output",
                "write_to_storage",
                "_write_to_local",
                "_write_to_lakehouse",
                "_validate_json",
                "_create_get_export_file_name",
            },
            {
                "os": SimpleNamespace(
                    path=SimpleNamespace(
                        normpath=os.path.normpath,
                        dirname=os.path.dirname,
                        join=os.path.join,
                        exists=lambda path: True,
                    )
                ),
                "json": json,
                "datetime": clock,
                "Namespace": SimpleNamespace,
                "open": open_output,
                "get_export_path": lambda path: events.append(("resolve", path))
                or {"type": "local", "path": path},
                "fab_ui": SimpleNamespace(
                    print_grey=lambda text, stderr: events.append(
                        ("notice", text, stderr)
                    ),
                    print_output_format=lambda args, **kwargs: events.append(
                        ("success", kwargs)
                    ),
                ),
                "onelake_api": SimpleNamespace(
                    touch_file=lambda args: events.append(("touch", args.to_path))
                    or SimpleNamespace(status_code=201),
                    append_file=lambda args, data, offset: events.append(
                        ("append", data, offset)
                    ),
                    flush_file=lambda args, position: events.append(
                        ("flush", position)
                    ),
                ),
            },
        )

    def test_projected_json_is_unicode_pretty_and_has_one_json_suffix(self) -> None:
        self.storage.do_output(
            {"name": "café 🚀", "order": [2, 1]},
            "Book.Notebook",
            SimpleNamespace(output="destination", force=False),
        )
        path = os.path.normpath(
            os.path.join("destination", "Book.Notebook", "get_20261102_135959.json")
        )
        self.assertEqual(
            self.content[path],
            '{\n    "name": "café 🚀",\n    "order": [\n        2,\n        1\n    ]\n}',
        )
        self.assertEqual(self.events[0], ("resolve", "destination"))
        self.assertEqual(self.events[1], ("clock", "%Y%m%d_%H%M%S"))
        notice_path = os.path.join(
            "destination/Book.Notebook", "get_20261102_135959.json"
        )
        self.assertEqual(
            self.events[2], ("notice", f"Exporting result to '{notice_path}'...", True)
        )
        self.assertEqual(
            self.events[3], ("open", path, "w", {"encoding": "utf-8"})
        )
        self.assertEqual(self.events[4], ("success", {"message": "Export completed"}))

    def test_empty_scalar_and_json_string_suffixes_ignore_force(self) -> None:
        for value, expected, suffix in [
            ("", "", ""),
            (False, "False", ""),
            (None, "None", ""),
            (0, "0", ""),
            ("false", "false", ".json"),
            ("null", "null", ".json"),
            ('"café"', '"café"', ".json"),
        ]:
            for force in [False, True]:
                self.storage.do_output(
                    value, "Book.Notebook",
                    SimpleNamespace(output="destination", force=force),
                )
                path = os.path.normpath(os.path.join(
                    "destination", "Book.Notebook", f"get_20261102_135959{suffix}"
                ))
                self.assertEqual(self.content[path], expected)
                self.assertEqual(self.events[-2][2], "w")

    def test_lakehouse_uses_ascii_json_and_discards_original_presentation_args(self) -> None:
        captured: list[Any] = []
        self.storage.fab_ui.print_output_format = (
            lambda args, **kwargs: captured.append((vars(args).copy(), kwargs))
        )
        self.storage.write_to_storage(
            SimpleNamespace(command="get", output_format="text"),
            {"type": "lakehouse", "path": "Demo.Workspace/Lake.Lakehouse/Files/result"},
            {"name": "café 🚀"},
        )
        expected = r'{"name": "caf\u00e9 \ud83d\ude80"}'
        self.assertEqual(self.events[1], ("append", expected, 0))
        self.assertEqual(self.events[2], ("flush", len(expected)))
        self.assertEqual(captured[0][0], {
            "to_path": "Demo/Lake.Lakehouse/Files/result.json"
        })
        self.assertEqual(captured[0][1], {"message": "Export completed"})

    def test_injected_non_utc_time_keeps_source_local_date(self) -> None:
        local = datetime(2026, 11, 2, 0, 30, tzinfo=timezone(timedelta(hours=2)))
        storage = load(
            "utils/fab_storage.py", {"_create_get_export_file_name"},
            {"os": os, "datetime": SimpleNamespace(now=lambda: local)},
        )
        self.assertEqual(
            storage._create_get_export_file_name("destination"),
            os.path.join("destination", "get_20261102_003000"),
        )
        self.assertEqual(
            local.astimezone(timezone.utc).strftime("%Y%m%d_%H%M%S"),
            "20261101_223000",
        )


if __name__ == "__main__":
    unittest.main()
