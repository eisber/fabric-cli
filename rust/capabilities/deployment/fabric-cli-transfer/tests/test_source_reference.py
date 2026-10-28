# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Verify the frozen independent source capture without rewriting expectations."""

import base64
import json
from pathlib import Path
import sys
import unittest

from source_reference import capture


class SourceContract(unittest.TestCase):
    def test_actual_handlers_match_frozen_receipt(self) -> None:
        expected = json.loads(
            Path(__file__).with_name("source_receipt.json").read_text(encoding="utf-8")
        )
        observed = capture()
        if expected["platform"] != sys.platform:
            expected["platform"] = sys.platform
            for case in expected["cases"]:
                for event in case["events"]:
                    if "<ROOT>" in event[1]:
                        event[1] = event[1].replace("\\", "/")
                for output in case["outputs"]:
                    if "message" in output and "<ROOT>" in output["message"]:
                        output["message"] = output["message"].replace("\\", "/")
                    for row in output.get("data", []):
                        if "output" in row:
                            row["output"] = row["output"].replace("\\", "/")
                if sys.platform != "win32":
                    case["files"] = {
                        path: base64.b64encode(
                            base64.b64decode(content).replace(b"\r\n", b"\n")
                        ).decode("ascii")
                        for path, content in case["files"].items()
                    }
        self.assertEqual(expected, observed)


if __name__ == "__main__":
    unittest.main()
