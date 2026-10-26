# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Fail-closed source-oracle countermeasures for text and definition encoders."""

import copy
import json
import unittest

from definition_reference import check as check_definitions
from reference import DIRECTORY
from text_reference import TextReference, check


class TextReferenceTests(unittest.TestCase):
    """Each observation is independently source-executed, never refreshed from Rust."""

    def test_handwritten_source_cases(self) -> None:
        self.assertGreater(check(), 50)
        self.assertEqual(check_definitions(), 12)

    def test_text_stream_exit_and_payload_mutations_fail(self) -> None:
        cases = json.loads((DIRECTORY / "text-cases.json").read_bytes())
        observed = [TextReference(case).observe() for case in cases]
        for key, value in (("stdout", "wrong\n"), ("stderr", "wrong\n"), ("exit", 4)):
            changed = copy.deepcopy(observed)
            index = next(
                i for i, item in enumerate(changed) if item["name"] == "raw-scalar"
            )
            changed[index][key] = value
            with self.subTest(key=key), self.assertRaises(AssertionError):
                check(changed)

    def test_definition_bytes_cannot_be_replaced_with_compact_semantic_proxy(
        self,
    ) -> None:
        cases = json.loads((DIRECTORY / "definition-cases.json").read_bytes())
        observed = [
            {"name": case["name"], "encoded": case["expected"]} for case in cases
        ]
        index = next(
            i for i, case in enumerate(cases) if case["name"] == "definition-order"
        )
        observed[index]["encoded"] = json.dumps(
            cases[index]["data"], separators=(",", ":")
        )
        with self.assertRaises(AssertionError):
            check_definitions(observed)


if __name__ == "__main__":
    unittest.main()
