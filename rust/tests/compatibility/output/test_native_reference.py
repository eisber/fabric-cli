# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Countermeasures for exact native Python values and non-JSON-domain distinctions."""

import copy
import json
import unittest

from native_reference import check, observe
from reference import DIRECTORY


class NativeReferenceTests(unittest.TestCase):
    """Never collapse runtime values through finite JSON or Unicode replacement."""

    def test_handwritten_native_source_cases(self) -> None:
        self.assertEqual(check(), 35)

    def test_nonfinite_integer_surrogate_and_pretty_mutations_fail(self) -> None:
        cases = json.loads((DIRECTORY / "native-cases.json").read_bytes())
        observations = [observe(case) for case in cases]
        changes = [
            ("wide-positive", "json", "1844674407370955200000000000000000000000"),
            ("nan-positive", "json", "null"),
            ("nan-negative", "repr", "-nan"),
            ("lone-high-surrogate", "str_codepoints", [65533]),
            ("surrogate-pair-not-scalar", "str_codepoints", [128640]),
            ("nan-positive", "truth", False),
            ("ordered-dict", "pretty2", '{"z": Infinity, "a": "\\ud800"}'),
        ]
        for name, key, replacement in changes:
            mutated = copy.deepcopy(observations)
            index = next(i for i, case in enumerate(cases) if case["name"] == name)
            mutated[index][key] = replacement
            with self.subTest(name=name, key=key), self.assertRaises(AssertionError):
                check(mutated)

    def test_case_completeness_is_required(self) -> None:
        with self.assertRaises(AssertionError):
            check([])

    def test_encoding_only_contract_keeps_exact_shape_and_rejects_loss(self) -> None:
        cases = json.loads((DIRECTORY / "native-cases.json").read_bytes())
        keys = ("name", "json", "pretty0", "pretty2", "pretty4", "truth", "unicode")
        observations = [
            {key: value for key, value in observe(case).items() if key in keys}
            for case in cases
        ]
        self.assertEqual(check(observations, encoding_only=True), len(cases))
        for name, key, replacement in [
            ("wide-positive", "json", "null"),
            ("lone-high-surrogate", "unicode", {"encoded": '"\ufffd"'}),
            ("duplicate-key-position", "json", '{"a": 2, "z": 3}'),
            ("surrogate-scalar-distinct-keys", "json", '{"\\ud83d\\ude80": 2}'),
        ]:
            actual = copy.deepcopy(observations)
            index = next(i for i, case in enumerate(cases) if case["name"] == name)
            actual[index][key] = replacement
            with self.subTest(name=name, key=key), self.assertRaises(AssertionError):
                check(actual, encoding_only=True)


if __name__ == "__main__":
    unittest.main()
