# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Oracle countermeasures: isolated namespaces and fail-closed comparison assertions."""

import copy
import json
import sys
import unittest

from reference import Reference, assert_observation, check_reference, fixtures


class ReferenceTests(unittest.TestCase):
    """Exercise the oracle without any Rust expected-output generation."""

    def test_source_matches_handwritten_cases(self) -> None:
        self.assertEqual(check_reference(), len(fixtures()["cases"]))

    def test_mutations_are_detected(self) -> None:
        inputs = fixtures()
        case = next(x for x in inputs["cases"] if x["name"] == "unauthorized")
        expected = Reference(inputs["timestamp"]).observe(case)
        mutations = []
        for key, value in [("stream", "stderr"), ("exit", 1), ("stderr", "error\n")]:
            changed = copy.deepcopy(expected)
            changed[key] = value
            mutations.append(changed)
        for action in ("remove-command", "remove-code", "change-message", "add-key"):
            changed = copy.deepcopy(expected)
            tree = json.loads(changed["stdout"])
            if action == "remove-command":
                del tree["command"]
            elif action == "remove-code":
                del tree["result"]["error_code"]
            elif action == "change-message":
                tree["result"]["message"] = "changed"
            else:
                tree["request_id"] = "not-in-product-json"
            changed["stdout"] = json.dumps(tree) + "\n"
            mutations.append(changed)
        changed = copy.deepcopy(expected)
        changed["stdout"] = changed["stdout"].replace("\n", "\r\n")
        mutations.append(changed)
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                with self.assertRaises(AssertionError):
                    assert_observation(case, mutation, expected)

    def test_namespaces_do_not_mutate_global_modules_or_share_streams(self) -> None:
        modules = dict(sys.modules)
        inputs = fixtures()
        first = Reference(inputs["timestamp"])
        second = Reference("2000-01-01T00:00:00Z")
        case = inputs["cases"][0]
        one = first.observe(case)
        two = second.observe(case)
        self.assertIn(inputs["timestamp"], one["stdout"])
        self.assertNotIn(inputs["timestamp"], two["stdout"])
        self.assertIsNot(first.namespace, second.namespace)
        self.assertEqual(modules, sys.modules)

    def test_unsupported_runtime_values_raise_exact_source_error(self) -> None:
        source = Reference(fixtures()["timestamp"]).namespace
        for unsupported in (object(), {1, 2}):
            output = source["FabricCLIOutput"](data=[unsupported])
            with self.assertRaises(source["FabricCLIError"]) as caught:
                output.to_json()
            self.assertEqual(caught.exception.status_code, "InvalidJson")
            self.assertEqual(caught.exception.message, "Invalid JSON format")

    def test_explicit_non_json_source_differences_are_not_silently_claimed(
        self,
    ) -> None:
        source = Reference(fixtures()["timestamp"]).namespace
        output = source["FabricCLIOutput"](data=[float("nan"), b"synthetic"])
        rendered = output.to_json()
        self.assertIn("NaN", rendered)
        self.assertIn('"__REDACTED__bytes__"', rendered)


if __name__ == "__main__":
    unittest.main()
