# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Countermeasures for exact source Unicode bytes, not semantic JSON alone."""

import copy
import json
import unittest

from reference import DIRECTORY
from unicode_reference import check


class UnicodeReferenceTests(unittest.TestCase):
    """Do not accept ASCII escaping, key sorting or newline normalization as parity."""

    def test_source_matches_new_handwritten_cases(self) -> None:
        self.assertEqual(check(), 18)

    def test_byte_only_and_legacy_mutations_fail(self) -> None:
        cases = json.loads((DIRECTORY / "unicode-json-cases.json").read_bytes())
        observed = [
            {
                "name": case["name"],
                "encoded": case["expected"],
                "ascii": json.dumps(case["data"], indent=case.get("indent", 4)),
            }
            for case in cases
        ]
        for name, key, replacement in [
            ("bmp-string", "encoded", r'"caf\u00e9 \u540d"'),
            ("del-not-escaped", "encoded", r'"\u007f"'),
            (
                "unicode-keys-order",
                "encoded",
                json.dumps(
                    cases[10]["data"], ensure_ascii=False, indent=4, sort_keys=True
                ),
            ),
            ("bmp-string", "ascii", cases[5]["expected"]),
            (
                "unicode-keys-order",
                "encoded",
                cases[10]["expected"].replace("\n", "\r\n"),
            ),
            ("empty-object", "encoded", "{}\n"),
        ]:
            actual = copy.deepcopy(observed)
            index = next(i for i, case in enumerate(cases) if case["name"] == name)
            actual[index][key] = replacement
            with self.subTest(name=name, key=key), self.assertRaises(AssertionError):
                check(actual)

    def test_missing_cases_fail_closed(self) -> None:
        with self.assertRaises(AssertionError):
            check([])

    def test_source_only_native_value_gaps_remain_explicit(self) -> None:
        self.assertEqual(json.dumps(float("nan"), ensure_ascii=False, indent=4), "NaN")
        self.assertEqual(
            json.dumps(2**80, ensure_ascii=False, indent=4), "1208925819614629174706176"
        )
        encoded = json.dumps("\ud800", ensure_ascii=False, indent=4)
        self.assertEqual([ord(character) for character in encoded], [34, 55296, 34])
        with self.assertRaises(UnicodeEncodeError):
            encoded.encode("utf-8")


if __name__ == "__main__":
    unittest.main()
