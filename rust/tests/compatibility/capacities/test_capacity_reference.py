# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Offline self-checks for source provenance, comparisons and execution guards."""

import copy
import json
import runpy
import sys
import unittest
from pathlib import Path
from typing import Any

DIRECTORY = Path(__file__).resolve().parent
REFERENCE = runpy.run_path(str(DIRECTORY / "reference.py"))


class CapacityReferenceTests(unittest.TestCase):
    """Exercise the oracle itself without changing files or executing a process."""

    def test_all_goldens_are_repeatable(self) -> None:
        """Every observation must match immutable expectations twice."""
        for case in REFERENCE["load_cases"]()["cases"]:
            with self.subTest(case=case["name"]):
                first = REFERENCE["observe"](case)
                second = REFERENCE["observe"](case)
                REFERENCE["assert_matches"](case, first)
                REFERENCE["assert_matches"](case, second)
                self.assertEqual(
                    json.dumps(first, sort_keys=True),
                    json.dumps(second, sort_keys=True),
                )

    def test_negative_control_observation_mutations(self) -> None:
        """Wrong data, types, metadata, requests, identity and delays must fail."""
        cases = {case["name"]: case for case in REFERENCE["load_cases"]()["cases"]}
        raw_case = cases["raw_optional_fields_unknown_types_and_service_order"]
        raw = REFERENCE["observe"](raw_case)
        mutations: list[tuple[str, dict[str, Any], dict[str, Any]]] = []

        wrong = copy.deepcopy(raw)
        wrong["value"].reverse()
        mutations.append(("row-order", raw_case, wrong))
        wrong = copy.deepcopy(raw)
        wrong["value"][3].pop("region")
        mutations.append(("null-versus-missing", raw_case, wrong))
        wrong = copy.deepcopy(raw)
        wrong["value"][4]["displayName"] = 17.0
        mutations.append(("integer-versus-float", raw_case, wrong))
        wrong = copy.deepcopy(raw)
        wrong["value"][0].pop("future")
        mutations.append(("unknown-field-loss", raw_case, wrong))
        wrong = copy.deepcopy(raw)
        wrong["value"][1]["state"] = "Active"
        mutations.append(("case-normalization", raw_case, wrong))
        wrong = copy.deepcopy(raw)
        wrong["requests"][0]["method"] = "post"
        mutations.append(("request-method", raw_case, wrong))
        wrong = copy.deepcopy(raw)
        wrong["requests"][0]["url"] = "https://management.azure.com/v1/capacities"
        mutations.append(("request-route", raw_case, wrong))
        wrong = copy.deepcopy(raw)
        wrong["scopes"][0][0] = "https://storage.azure.com/.default"
        mutations.append(("credential-scope", raw_case, wrong))

        page_case = cases["body_tokens_three_pages_empty_middle_opaque_encoding"]
        pages = REFERENCE["observe"](page_case)
        wrong = copy.deepcopy(pages)
        wrong["total_pages"] = 3
        mutations.append(("python-page-count-quirk", page_case, wrong))
        wrong = copy.deepcopy(pages)
        wrong["requests"][1]["url"] = wrong["requests"][1]["url"].replace("%2B", "+")
        mutations.append(("opaque-token-encoding", page_case, wrong))
        wrong = copy.deepcopy(pages)
        wrong["response_json"]["future"] = {"second": True}
        mutations.append(("first-envelope-preservation", page_case, wrong))

        retry_case = cases["rate_limit_exhaustion_sleeps_even_after_fourth_response"]
        retry = REFERENCE["observe"](retry_case)
        wrong = copy.deepcopy(retry)
        wrong["sleeps"].pop()
        mutations.append(("final-attempt-sleep", retry_case, wrong))
        wrong = copy.deepcopy(retry)
        wrong["error_code"] = "429"
        mutations.append(("error-code", retry_case, wrong))
        wrong = copy.deepcopy(retry)
        wrong["scopes"].append(wrong["scopes"][0])
        mutations.append(("per-attempt-auth-instead-of-per-page", retry_case, wrong))
        wrong = copy.deepcopy(retry)
        wrong["requests"].pop()
        mutations.append(("retry-count", retry_case, wrong))

        consumer_case = cases[
            "consumer_required_strings_raw_optional_fields_and_unknown_enums"
        ]
        consumer = REFERENCE["observe"](consumer_case)
        wrong = copy.deepcopy(consumer)
        wrong["value"][3].pop("sku")
        mutations.append(("sku-null-versus-missing", consumer_case, wrong))
        wrong = copy.deepcopy(consumer)
        wrong["value"][4]["sku"] = "17"
        mutations.append(("raw-sku-number-versus-string", consumer_case, wrong))
        wrong = copy.deepcopy(consumer)
        wrong["value"][2].pop("displayName")
        mutations.append(("required-display-name-loss", consumer_case, wrong))

        created_case = cases["python_quirk_terminal_second_page_201_is_not_merged"]
        created = REFERENCE["observe"](created_case)
        wrong = copy.deepcopy(created)
        wrong["value"].append({"id": "second", "displayName": "Second"})
        mutations.append(("source-201-page-is-not-merged", created_case, wrong))
        wrong = copy.deepcopy(created)
        wrong["response_json"].pop("continuationToken")
        mutations.append(("source-201-retains-first-token", created_case, wrong))
        for label, case, observation in mutations:
            with self.subTest(mutation=label):
                with self.assertRaises(AssertionError):
                    REFERENCE["assert_matches"](case, observation)

    def test_rust_parity_metadata_respects_consumer_requirements(self) -> None:
        """Parity labels need consumer-valid rows, not weaker factory acceptance."""
        for case in REFERENCE["load_cases"]()["cases"]:
            with self.subTest(case=case["name"]):
                if not case["rust_parity"]:
                    self.assertIsInstance(case["rust_exception"], str)
                    self.assertTrue(case["rust_exception"])
                    continue
                for response in case["responses"]:
                    if response["status"] not in (200, 201):
                        continue
                    body = response["body"]
                    self.assertIsInstance(body, dict)
                    self.assertIsInstance(body["value"], list)
                    for row in body["value"]:
                        self.assertIsInstance(row, dict)
                        self.assertIsInstance(row["id"], str)
                        self.assertIsInstance(row["displayName"], str)

    def test_source_hash_negative_controls(self) -> None:
        """Every source hash and the exact source-file set fail closed."""
        sources = REFERENCE["load_sources"]()
        for relative in sources:
            with self.subTest(source=relative):
                changed = dict(sources)
                changed[relative] += b"\n# source drift negative control\n"
                with self.assertRaises(AssertionError):
                    REFERENCE["verify_sources"](changed)
        missing = dict(sources)
        missing.pop(next(iter(missing)))
        with self.assertRaises(AssertionError):
            REFERENCE["verify_sources"](missing)
        additional = dict(sources)
        additional["unreviewed.py"] = b""
        with self.assertRaises(AssertionError):
            REFERENCE["verify_sources"](additional)

    def test_fixture_hash_negative_control(self) -> None:
        """Even harmless fixture edits require explicit independent review."""
        contents = (DIRECTORY / "cases.json").read_bytes()
        with self.assertRaises(AssertionError):
            REFERENCE["verify_cases"](contents + b"\n")
        changed = contents.replace(b'"method": "get"', b'"method": "post"', 1)
        self.assertNotEqual(contents, changed)
        with self.assertRaises(AssertionError):
            REFERENCE["verify_cases"](changed)

    def test_crlf_normalization_is_the_only_hash_tolerance(self) -> None:
        """CRLF checkouts remain pinned without ignoring other whitespace."""
        digest = REFERENCE["normalized_sha256"]
        self.assertEqual(digest(b"one\ntwo\n"), digest(b"one\r\ntwo\r\n"))
        self.assertNotEqual(digest(b"one\n"), digest(b"one \n"))
        self.assertNotEqual(digest(b"one\n"), digest(b"\xef\xbb\xbfone\n"))

    def test_audit_boundary_probes_do_not_perform_operations(self) -> None:
        """Synthetic audit events prove guard coverage with no actual I/O."""
        for event, arguments in [
            ("open", ("forbidden", "r", 0)),
            ("open", ("forbidden", "w", 0)),
            ("socket.connect", (None, ("synthetic.invalid", 443))),
            ("socket.getaddrinfo", ("synthetic.invalid", 443, 0, 0, 0)),
            ("subprocess.Popen", ("forbidden", [], None, None)),
            ("os.system", ("forbidden",)),
            ("os.mkdir", ("forbidden", 0, -1)),
            ("os.remove", ("forbidden", -1)),
            ("ctypes.dlopen", ("forbidden",)),
        ]:
            with self.subTest(event=event, arguments=arguments):
                with REFERENCE["source_guard"]() as violations:
                    with self.assertRaises(REFERENCE["OfflineViolation"]):
                        sys.audit(event, *arguments)
                    self.assertEqual(violations, [event])

    def test_import_and_builtin_boundaries(self) -> None:
        """Source imports resolve only fake auth/context; I/O builtins fail."""
        sources = REFERENCE["load_sources"]()
        case = REFERENCE["load_cases"]()["cases"][0]
        with REFERENCE["source_guard"]():
            namespace, _, _, scopes = REFERENCE["_source_namespace"](case, sources)
            source_builtins = namespace["__builtins__"]
            source_import = source_builtins["__import__"]
            for name in (
                "socket",
                "subprocess",
                "os",
                "time",
                "pathlib",
                "requests",
                "msal",
                "msal_extensions",
                "azure.identity",
                "fabric_cli.core.fab_auth",
            ):
                with self.subTest(import_name=name):
                    with self.assertRaises(REFERENCE["OfflineViolation"]):
                        source_import(name)
            auth = source_import("fabric_cli.core.fab_auth", fromlist=("FabAuth",))
            self.assertEqual(
                auth.FabAuth().get_access_token(["synthetic-scope"]),
                "synthetic-offline-token",
            )
            self.assertEqual(scopes, [["synthetic-scope"]])
            self.assertNotIn("fabric_cli.core.fab_auth", sys.modules)
            for name, args in (
                ("open", ("forbidden", "w")),
                ("input", ()),
                ("print", ("forbidden",)),
            ):
                with self.subTest(builtin=name):
                    with self.assertRaises(REFERENCE["OfflineViolation"]):
                        source_builtins[name](*args)

    def test_unscripted_and_unused_http_are_rejected(self) -> None:
        """Script bounds prevent extra calls or ignored scripted failures."""
        case = copy.deepcopy(REFERENCE["load_cases"]()["cases"][0])
        case["responses"] = []
        with self.assertRaisesRegex(AssertionError, "unscripted response"):
            REFERENCE["observe"](case)
        case = copy.deepcopy(REFERENCE["load_cases"]()["cases"][0])
        case["responses"].append(copy.deepcopy(case["responses"][0]))
        with self.assertRaisesRegex(AssertionError, "unused scripted responses"):
            REFERENCE["observe"](case)


if __name__ == "__main__":
    unittest.main()
