# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Synthetic harness contracts only: no Cargo, product binaries, network or credentials."""

from __future__ import annotations

import copy
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import threading
import unittest
from unittest.mock import patch
from typing import Any
import uuid

import docs_search as benchmark


class DocsSearchContracts(unittest.TestCase):
    """Keep provenance, failures, relevance and actual measurement units independent."""

    def setUp(self) -> None:
        self.directory = Path.cwd() / (".docs-search-tests-" + uuid.uuid4().hex)
        self.directory.mkdir()
        self.addCleanup(shutil.rmtree, self.directory)
        fixture, _ = benchmark.read_json(benchmark.DEFAULT_QUERIES, benchmark.MAX_FIXTURE_BYTES)
        self.fixture = fixture
        # A positive and negative control suffice for synthetic runner coverage.
        self.fixture["cases"] = [fixture["cases"][0], fixture["cases"][-1]]
        self.cases = self.fixture["cases"]
        self.queries = self.directory / "queries.json"
        self.queries.write_text(json.dumps(self.fixture), encoding="utf-8")
        self.manifest = {
            "schema_version": 1,
            "schema_id": "fabric-docs-sections-v1",
            "source_commit": benchmark.SOURCE_COMMIT,
            "documents": 100,
            "source_markdown_files": 3258,
            "source_markdown_bytes": 26329743,
            "compression": "lz4",
            "files": [{"name": "segment.idx", "bytes": 42, "sha256": "b" * 64}],
        }
        self.timings = {"parse_us": 1, "search_us": 2, "fetch_us": 3, "total_us": 7}
        self.targets = {}
        variants = {}
        for index, name in enumerate(("baseline", "engine_only", "embedded", "reader")):
            path = self.directory / (name + ".exe")
            path.write_bytes(b"synthetic-not-an-executable" + b"x" * index)
            path.chmod(0o755)
            self.targets[name] = path
            variants[name] = {
                "source_sha": "a" * 40,
                "sha256": benchmark.artifact(path)["sha256"],
                "features": ["docs-search"] if name in ("engine_only", "embedded") else [],
            }
        self.metadata = {
            "schema_version": 1, "toolchain": "synthetic rustc",
            "target_triple": "synthetic-target", "profile": "release",
            "receipt": "synthetic-build-evidence", "variants": variants,
        }
        self.metadata_path = self.directory / "metadata.json"
        self.write_metadata()
        self.output = self.directory / "receipt.json"
        self.args = [
            "--baseline", str(self.targets["baseline"]),
            "--engine-only", str(self.targets["engine_only"]),
            "--embedded", str(self.targets["embedded"]),
            "--reader-benchmark", str(self.targets["reader"]),
            "--build-metadata", str(self.metadata_path),
            "--source-sha", "a" * 40,
            "--execution-grant", "synthetic-admission-not-a-real-grant",
            "--queries", str(self.queries),
            "--output", str(self.output),
            "--samples", "2", "--warmups", "1", "--iterations", "2",
        ]
        self.calls: list[tuple[str, list[str], float, Path, dict[str, str]]] = []

    def write_metadata(self) -> None:
        """Persist only this test's synthetic metadata in its workspace artifact."""
        self.metadata_path.write_text(json.dumps(self.metadata), encoding="utf-8")

    def hits(self, case: dict[str, Any]) -> list[dict[str, Any]]:
        """Supply a fixed synthetic source result, not live-derived expectations."""
        if case["id"] == "no-hit-sentinel":
            return []
        return [{
            "id": "docs/onelake/onelake-shortcuts.md#synthetic",
            "source_path": "docs/onelake/onelake-shortcuts.md",
            "title": "Synthetic shortcuts", "heading": "Synthetic heading",
            "url": "https://learn.microsoft.com/fabric/onelake/onelake-shortcuts",
            "score": 2.0, "snippet": "Synthetic OneLake shortcuts fragment.",
        }]

    def search_result(self, case: dict[str, Any]) -> dict[str, Any]:
        """Create a product-shaped JSON envelope with synthetic measured values."""
        return {
            "status": "succeeded", "source": "embeddedFabricDocs", "data": self.hits(case),
            "meta": {
                "sourceCommit": benchmark.SOURCE_COMMIT,
                "indexedSections": 100, "resultLimit": case["limit"],
                "openUs": 11, "timings": dict(self.timings),
            },
        }

    def reader_report(self, mode: str = "embedded") -> dict[str, Any]:
        """Create independently recomputable samples for the same-process schema."""
        rows = []
        for case in self.cases:
            hits = self.hits(case)
            sample = {
                "wall_us": 9, "timings": dict(self.timings),
                "observed_ids": [hit["id"] for hit in hits],
                "observed_source_paths": [hit["source_path"] for hit in hits],
                "expected_source_hit": True,
            }
            rows.append({
                **case, "warmup": copy.deepcopy(sample), "warmup_top_hits": hits,
                "observed_top_hits": hits,
                "samples": [copy.deepcopy(sample), copy.deepcopy(sample)],
                "distributions_us": {
                    key: benchmark.summary([value, value])
                    for key, value in (("wall", 9), ("parse", 1), ("search", 2), ("fetch", 3), ("total", 7))
                },
            })
        return {
            "schema_version": 1, "status": "passed", "mode": mode,
            "iterations": 2, "units": "microseconds", "source_commit": benchmark.SOURCE_COMMIT,
            "open_us": 12, "manifest": copy.deepcopy(self.manifest), "cases": rows,
        }

    def fake_invoke(
        self, executable: Path, arguments: list[str], timeout: float, home: Path
    ) -> tuple[dict[str, Any], bytes]:
        """Do not execute the synthetic files; emulate their bounded output streams."""
        name = next(name for name, path in self.targets.items() if path == executable)
        self.calls.append((name, arguments, timeout, home, benchmark.isolated_environment(home)))
        exit_code = 0
        stderr = b""
        if arguments == ["--version"]:
            raw = benchmark.VERSION
        elif arguments == ["--help"]:
            raw = b"Usage: fab-rs [OPTIONS]\n --help --version\n"
        elif name == "reader":
            report = self.reader_report("copied_ram" if "--copy" in arguments else "embedded")
            raw = json.dumps(report).encode()
            exit_code = 0 if report["status"] == "passed" else 1
        elif "--corpus-info" in arguments:
            if name in ("baseline", "engine_only"):
                raw = b""
                exit_code = 2 if name == "baseline" else 1
                stderr = json.dumps(benchmark.VARIANT_ERRORS[name]).encode() + b"\n"
            else:
                raw = json.dumps({
                    "status": "succeeded", "source": "embeddedFabricDocs", "data": self.manifest,
                }).encode()
        else:
            text = arguments[arguments.index("--text") + 1]
            case = next(case for case in self.cases if case["text"] == text)
            raw = json.dumps(self.search_result(case)).encode()
        return {
            "wall_ms": 2.5, "exit_code": exit_code, "stdout_bytes": len(raw), "stderr_bytes": len(stderr),
            "stdout_sha256": hashlib.sha256(raw).hexdigest(),
            "stderr_preview": stderr.decode(), "stderr_truncated": False, "failure": None,
        }, raw

    def execute(self, extra: list[str] | None = None) -> tuple[int, dict[str, Any]]:
        """Invoke only the harness, with all benchmark processes mocked."""
        with patch.object(benchmark, "invoke", side_effect=self.fake_invoke):
            status = benchmark.main([*self.args, *(extra or [])])
        return status, json.loads(self.output.read_text(encoding="utf-8"))

    def test_default_fixture_is_pinned_grounded_and_covers_query_families(self) -> None:
        value, _ = benchmark.read_json(benchmark.DEFAULT_QUERIES, benchmark.MAX_FIXTURE_BYTES)
        cases = benchmark.load_queries(value)
        self.assertEqual(value["source_commit"], benchmark.SOURCE_COMMIT)
        texts = [case["text"] for case in cases]
        self.assertIn('"workspace identity"~2', texts)
        self.assertTrue(any(" OR " in text and " AND " in text for text in texts))
        self.assertIn("identifiers:spark.sql.parquet.vorder.default", texts)
        self.assertTrue(any(case["fuzzy"] == 1 and case["text"] == "onlake" for case in cases))
        self.assertEqual(cases[-1]["expected"], {"kind": "no_hits", "value": None})

    def test_fixture_schema_bounds_and_expectations_reject_invalid_values(self) -> None:
        mutations = [
            lambda value: value.update(schema_version=True),
            lambda value: value.update(source_commit="0" * 40),
            lambda value: value.update(cases=[]),
            lambda value: value["cases"].append(copy.deepcopy(value["cases"][0])),
            lambda value: value["cases"][0].update(text=" " * 2049),
            lambda value: value["cases"][0].update(text="x " * 65),
            lambda value: value["cases"][0].update(text="(((((((((x)))))))))"),
            lambda value: value["cases"][0].update(text="(x"),
            lambda value: value["cases"][0].update(text="x*"),
            lambda value: value["cases"][0].update(text='"x"', fuzzy=1),
            lambda value: value["cases"][0].update(limit=True),
            lambda value: value["cases"][0].update(limit=51),
            lambda value: value["cases"][0].update(workload="Security"),
            lambda value: value["cases"][0].update(fuzzy=3),
            lambda value: value["cases"][0].update(grounding=""),
            lambda value: value["cases"][0]["expected"].update(value="docs/../secret.md"),
            lambda value: value["cases"][0]["expected"].update(kind="learn-from-results"),
            lambda value: value["cases"][-1]["expected"].update(value="not-null"),
        ]
        for mutate in mutations:
            value = copy.deepcopy(self.fixture)
            mutate(value)
            with self.subTest(value=value), self.assertRaises(benchmark.BenchmarkError):
                benchmark.load_queries(value)

    def test_fixture_nesting_ignores_quoted_and_escaped_parentheses(self) -> None:
        for text in [
            '"COUNT("', "'COUNT('", r"COUNT\(", r"COUNT\)",
            r"COUNT\\\(", r'"say \"COUNT("', r"'say \'COUNT('",
            """("COUNT(" OR 'SUM)')""",
            '(((((((("COUNT((((((((("))))))))',
        ]:
            value = copy.deepcopy(self.fixture)
            value["cases"][0].update(text=text, fuzzy=0)
            with self.subTest(text=text):
                self.assertEqual(benchmark.load_queries(value)[0]["text"], text)
        for text in [
            r"COUNT\\(", '"COUNT(', "'COUNT(", "COUNT\\",
            '("COUNT("', '"COUNT(")', '((((((((("COUNT(")))))))))',
        ]:
            value = copy.deepcopy(self.fixture)
            value["cases"][0].update(text=text, fuzzy=0)
            with self.subTest(text=text), self.assertRaises(benchmark.BenchmarkError):
                benchmark.load_queries(value)

    def test_summary_retains_outliers_and_uses_nearest_rank_p95(self) -> None:
        values = list(range(1, 31))
        values[-1] = 10000
        self.assertEqual(benchmark.summary(values), {
            "samples": 30, "p50": 15.5, "p95": 29, "min": 1, "max": 10000,
        })
        for invalid in ([], [-1], [True], [float("nan")], [float("inf")], [10**400]):
            with self.subTest(values=invalid), self.assertRaises(benchmark.BenchmarkError):
                benchmark.summary(invalid)

    def test_json_rejects_duplicates_nonfinite_and_non_utf8(self) -> None:
        for raw in (b'{"x":1,"x":2}', b'{"x":NaN}', b'{"x":1e999}', b"\xff"):
            with self.subTest(raw=raw), self.assertRaises((ValueError, UnicodeError)):
                benchmark.decode_json(raw)
        self.queries.write_bytes(b"x" * (benchmark.MAX_FIXTURE_BYTES + 1))
        with self.assertRaisesRegex(benchmark.BenchmarkError, "byte budget"):
            benchmark.read_json(self.queries, benchmark.MAX_FIXTURE_BYTES)

    def test_environment_uses_explicit_profile_and_excludes_ambient_secrets(self) -> None:
        with patch.dict(os.environ, {
            "FAB_TOKEN": "synthetic", "AZURE_CLIENT_SECRET": "synthetic",
            "HTTPS_PROXY": "synthetic", "PATH": "credential-bearing-path",
            "PYTHONPATH": "synthetic", "FAB_RUST_CONFIG_ROOT": "ambient",
        }):
            environment = benchmark.isolated_environment(self.directory)
        for name in ("FAB_TOKEN", "AZURE_CLIENT_SECRET", "HTTPS_PROXY", "PYTHONPATH"):
            self.assertNotIn(name, environment)
        self.assertEqual(environment["PATH"], "")
        self.assertEqual(environment["HOME"], str(self.directory))
        self.assertEqual(environment["TEMP"], str(self.directory))
        self.assertTrue(environment["FAB_RUST_CONFIG_ROOT"].startswith(str(self.directory)))

    def test_complete_run_separates_startup_queries_reader_and_ram_control(self) -> None:
        status, receipt = self.execute(["--ram-control"])
        self.assertEqual(status, 0)
        self.assertEqual(receipt["status"], "passed")
        self.assertEqual(receipt["shuffle_seed"], benchmark.SEED)
        self.assertEqual(receipt["index_component_bytes"], 42)
        self.assertEqual(receipt["artifacts_before"], receipt["artifacts_after"])
        self.assertEqual(receipt["variant_states"], {
            "baseline": "docs_search_command_absent",
            "engine_only": "docs_search_engine_without_corpus",
            "embedded": "embedded_corpus_verified",
        })
        self.assertEqual(
            [(row["target"], row["exit_code"]) for row in receipt["observations"][:3]],
            [("baseline", 2), ("engine_only", 1), ("embedded", 0)],
        )
        self.assertEqual(receipt["executable_byte_deltas"], {
            "engine_only_minus_baseline": 1, "embedded_minus_engine_only": 1,
            "embedded_minus_baseline": 2,
        })
        startup = [row for row in receipt["observations"] if row["kind"] == "startup"]
        self.assertEqual(len(startup), 3 * 2 * (1 + 1 + 2))
        self.assertEqual({row["phase"] for row in startup}, {"first_observed", "warmup", "sample"})
        self.assertEqual(len(receipt["startup_summaries"]), 6)
        self.assertTrue(all(row["wall_ms"]["samples"] == 2 for row in receipt["startup_summaries"]))
        self.assertEqual(receipt["query_summaries"][0]["open_us"]["p50"], 11)
        self.assertEqual(receipt["query_summaries"][0]["timings_us"]["total_us"]["samples"], 2)
        self.assertEqual(set(receipt["reader_reports"]), {"embedded", "copied_ram"})
        self.assertEqual(receipt["reader_reports"]["embedded"]["open_us"], 12)
        self.assertFalse(any("--copy" in args for name, args, *_ in self.calls if name != "reader"))
        self.assertTrue(all(0 < timeout <= 15 for _, _, timeout, *_ in self.calls))
        self.assertTrue(all(not home.exists() for _, _, _, home, _ in self.calls))

    def test_fixed_seed_schedule_is_repeatable_and_interleaved(self) -> None:
        self.execute()
        first = [(name, args) for name, args, *_ in self.calls]
        self.output.unlink()
        self.calls.clear()
        self.execute()
        self.assertEqual(first, [(name, args) for name, args, *_ in self.calls])
        self.assertEqual({name for name, _ in first[:6]}, {"baseline", "engine_only", "embedded"})
        self.assertNotEqual(first[:6], first[6:12])

    def test_help_checks_do_not_learn_or_require_cross_variant_byte_snapshots(self) -> None:
        original = self.fake_invoke

        def invoke(
            executable: Path, arguments: list[str], timeout: float, home: Path
        ) -> tuple[dict[str, Any], bytes]:
            row, raw = original(executable, arguments, timeout, home)
            if arguments == ["--help"]:
                raw += executable.name.encode() + b"\n"
                row["stdout_bytes"] = len(raw)
                row["stdout_sha256"] = hashlib.sha256(raw).hexdigest()
            return row, raw

        with patch.object(benchmark, "invoke", side_effect=invoke):
            self.assertEqual(benchmark.main(self.args), 0)
        receipt = json.loads(self.output.read_text(encoding="utf-8"))
        rows = [row for row in receipt["observations"] if row["kind"] == "startup" and row["arguments"] == ["--help"]]
        self.assertEqual(len({row["stdout_sha256"] for row in rows}), 3)

    def test_rich_manifest_capabilities_and_catalog_are_explicit(self) -> None:
        manifest = copy.deepcopy(self.manifest)
        manifest["schema_id"] = "fabric-docs-sections-v2"
        manifest["features"] = dict(stemmed=True, metadata=True, hierarchy=True, autocomplete=True)
        manifest["filter_catalog"] = {"ms.topic": {"concept-article": 10}}
        self.assertEqual(benchmark.validate_manifest(manifest), manifest)
        for mutation in [
            lambda value: value.pop("features"),
            lambda value: value["features"].update(stemmed="enabled"),
            lambda value: value["filter_catalog"]["ms.topic"].update({"concept-article": 0}),
            lambda value: value.update(schema_id="guessed-future-schema"),
        ]:
            broken = copy.deepcopy(manifest)
            mutation(broken)
            with self.assertRaises(benchmark.BenchmarkError):
                benchmark.validate_manifest(broken)

    def test_manifest_digest_pin_is_verified_before_queries(self) -> None:
        status, _ = self.execute(["--manifest-sha256", benchmark.canonical_digest(self.manifest)])
        self.assertEqual(status, 0)
        self.output.unlink()
        self.calls.clear()
        status, receipt = self.execute(["--manifest-sha256", "0" * 64])
        self.assertEqual(status, 1)
        self.assertIn("canonical digest", receipt["error"])
        self.assertFalse(any("--text" in args for _, args, *_ in self.calls))
        self.assertIn("artifacts_after", receipt)

    def test_source_judgments_do_not_require_rank_one_and_no_hit_is_real(self) -> None:
        source = "docs/onelake/onelake-shortcuts.md"
        self.assertTrue(benchmark.judge(["docs/onelake/other.md", source], self.cases[0]["expected"]))
        self.assertFalse(benchmark.judge(["docs/onelake/other.md"], self.cases[0]["expected"]))
        self.assertTrue(benchmark.judge([source], {"kind": "source_prefix", "value": "docs/onelake/"}))
        self.assertTrue(benchmark.judge([], {"kind": "no_hits", "value": None}))
        self.assertFalse(benchmark.judge([source], {"kind": "no_hits", "value": None}))

    def test_source_expectations_accept_exact_document_extensions_and_safe_paths(self) -> None:
        for extension in ("md", "yml", "yaml"):
            path = f"docs/onelake/index.{extension}"
            fixture = copy.deepcopy(self.fixture)
            fixture["cases"][0]["expected"]["value"] = path
            with self.subTest(path=path):
                cases = benchmark.load_queries(fixture)
                self.assertEqual(cases[0]["expected"]["value"], path)
        for path in (
            "docs/onelake/index.MD", "docs/onelake/index.YML",
            "docs/onelake/index.mdx", "docs/onelake/index.ymll",
            "docs/onelake/index.yaml.gz", "docs/onelake/index",
            "/docs/onelake/index.yml", "other/onelake/index.yaml",
            "docs/../index.yml", "docs/./index.yaml", "docs//index.yml",
            "docs\\onelake\\index.yaml",
        ):
            fixture = copy.deepcopy(self.fixture)
            fixture["cases"][0]["expected"]["value"] = path
            with self.subTest(path=path), self.assertRaises(benchmark.BenchmarkError):
                benchmark.load_queries(fixture)
        self.assertEqual(
            self.cases[0]["expected"]["value"], "docs/onelake/onelake-shortcuts.md"
        )

    def test_yaml_result_shapes_do_not_relax_markdown_relevance_expectations(self) -> None:
        for extension in ("yml", "yaml"):
            result = self.search_result(self.cases[0])
            landing = dict(
                result["data"][0],
                id=f"docs/onelake/index.{extension}#landing",
                source_path=f"docs/onelake/index.{extension}",
            )
            result["data"].insert(0, landing)
            with self.subTest(extension=extension):
                self.assertTrue(
                    benchmark.validate_search(result, self.cases[0], self.manifest)
                )
                result["data"] = [landing]
                self.assertFalse(
                    benchmark.validate_search(result, self.cases[0], self.manifest)
                )

    def test_reader_accepts_yaml_hits_alongside_expected_markdown_hit(self) -> None:
        for extension in ("yml", "yaml"):
            report = self.reader_report()
            row = report["cases"][0]
            landing = dict(
                row["observed_top_hits"][0],
                id=f"docs/onelake/index.{extension}#landing",
                source_path=f"docs/onelake/index.{extension}",
            )
            for key in ("warmup_top_hits", "observed_top_hits"):
                row[key] = [landing, *row[key]]
            for sample in [row["warmup"], *row["samples"]]:
                sample["observed_ids"].insert(0, landing["id"])
                sample["observed_source_paths"].insert(0, landing["source_path"])
            with self.subTest(extension=extension):
                self.assertTrue(
                    benchmark.validate_reader(
                        report, self.cases, 2, "embedded", self.manifest
                    )
                )

    def test_false_cli_relevance_is_preserved_and_exits_nonzero(self) -> None:
        original = self.search_result

        def result(case: dict[str, Any]) -> dict[str, Any]:
            payload = original(case)
            if case["id"] == "onelake-shortcuts":
                payload["data"] = []
            return payload

        with patch.object(self, "search_result", side_effect=result):
            status, receipt = self.execute()
        self.assertEqual(status, 1)
        self.assertEqual(receipt["status"], "failed_relevance")
        rows = [row for row in receipt["observations"] if row["kind"] == "query" and row["case_id"] == "onelake-shortcuts"]
        self.assertTrue(all(row["expected_source_hit"] is False for row in rows))
        self.assertTrue(all(row["valid"] for row in rows))
        self.assertEqual(rows[0]["expected"]["value"], "docs/onelake/onelake-shortcuts.md")

    def test_reader_false_relevance_is_retained_even_with_nonzero_exit(self) -> None:
        original = self.reader_report

        def report(mode: str = "embedded") -> dict[str, Any]:
            value = original(mode)
            value["status"] = "failed_relevance"
            row = value["cases"][0]
            row["warmup_top_hits"] = row["observed_top_hits"] = []
            for sample in [row["warmup"], *row["samples"]]:
                sample.update(observed_ids=[], observed_source_paths=[], expected_source_hit=False)
            return value

        with patch.object(self, "reader_report", side_effect=report):
            status, receipt = self.execute()
        self.assertEqual(status, 1)
        self.assertEqual(receipt["status"], "failed_relevance")
        self.assertEqual(receipt["reader_reports"]["embedded"]["status"], "failed_relevance")
        row = next(row for row in receipt["observations"] if row["kind"] == "reader")
        self.assertEqual(row["exit_code"], 1)
        self.assertTrue(row["valid"])

    def test_reader_rejects_false_judgment_bad_shapes_and_fabricated_aggregates(self) -> None:
        mutations = [
            lambda value: value.update(iterations=3),
            lambda value: value.update(units="milliseconds"),
            lambda value: value["manifest"].update(documents=101),
            lambda value: value["cases"][0]["samples"].pop(),
            lambda value: value["cases"][0]["samples"][0].update(expected_source_hit=False),
            lambda value: value["cases"][0]["samples"][0].update(observed_source_paths=[]),
            lambda value: value["cases"][0]["samples"][0]["timings"].update(total_us=1),
            lambda value: value["cases"][0]["distributions_us"]["wall"].update(p95=1),
            lambda value: value["cases"][0].update(text="different query"),
            lambda value: value.update(status="failed_relevance"),
        ]
        for mutate in mutations:
            value = self.reader_report()
            mutate(value)
            with self.subTest(value=value), self.assertRaises(benchmark.BenchmarkError):
                benchmark.validate_reader(value, self.cases, 2, "embedded", self.manifest)

    def test_search_shapes_identity_filter_and_stage_durations_are_checked(self) -> None:
        mutations = [
            lambda value: value.update(status="failed"),
            lambda value: value.update(source="network"),
            lambda value: value.update(data={}),
            lambda value: value["meta"].update(sourceCommit="f" * 40),
            lambda value: value["meta"].update(indexedSections=True),
            lambda value: value["meta"].update(resultLimit=1),
            lambda value: value["meta"].update(openUs=-1),
            lambda value: value["meta"]["timings"].update(parse_us=True),
            lambda value: value["meta"]["timings"].update(total_us=2),
            lambda value: value["data"][0].update(score=float("inf")),
            lambda value: value["data"][0].update(source_path="docs/../outside.md"),
            lambda value: value["data"][0].pop("snippet"),
            lambda value: value["data"].append(value["data"][0]),
        ]
        for mutate in mutations:
            value = self.search_result(self.cases[0])
            mutate(value)
            with self.subTest(value=value), self.assertRaises(benchmark.BenchmarkError):
                benchmark.validate_search(value, self.cases[0], self.manifest)
        scoped = dict(self.cases[0], workload="security")
        with self.assertRaisesRegex(benchmark.BenchmarkError, "workload"):
            benchmark.validate_search(self.search_result(scoped), scoped, self.manifest)

    def test_failure_retains_nonzero_exit_wrong_version_stderr_and_bad_json(self) -> None:
        original = self.fake_invoke
        for failure in ("exit", "version", "stderr", "json"):
            self.output.unlink(missing_ok=True)

            def invoke(
                executable: Path, arguments: list[str], timeout: float, home: Path
            ) -> tuple[dict[str, Any], bytes]:
                row, raw = original(executable, arguments, timeout, home)
                if failure == "exit":
                    row["exit_code"] = 9
                elif failure == "version" and arguments == ["--version"]:
                    raw = b"fab-rs fabricated-version\n"
                elif failure == "stderr":
                    row["stderr_bytes"] = 4
                    row["stderr_preview"] = "oops"
                elif failure == "json" and "--corpus-info" in arguments:
                    raw = b'{"not":"complete"'
                return row, raw

            with (
                patch.object(benchmark, "invoke", side_effect=invoke),
                self.subTest(failure=failure),
            ):
                self.assertEqual(benchmark.main(self.args), 1)
            receipt = json.loads(self.output.read_text(encoding="utf-8"))
            self.assertEqual(receipt["status"], "failed")
            self.assertIn("error", receipt)
            self.assertNotIn("startup_summaries", receipt)
            self.assertFalse(receipt["observations"][-1]["valid"])

    def test_no_overwrite_even_on_failure_and_no_process_for_invalid_inputs(self) -> None:
        self.output.write_text("existing evidence", encoding="utf-8")
        with patch.object(benchmark, "invoke") as invoke:
            with self.assertRaises(FileExistsError):
                benchmark.main(self.args)
            invoke.assert_not_called()
        self.assertEqual(self.output.read_text(encoding="utf-8"), "existing evidence")
        self.output.unlink()
        self.targets["embedded"].write_bytes(b"changed")
        with patch.object(benchmark, "invoke") as invoke:
            self.assertEqual(benchmark.main(self.args), 1)
            invoke.assert_not_called()
        receipt = json.loads(self.output.read_text(encoding="utf-8"))
        self.assertIn("digest", receipt["error"])

    def test_preflight_rejects_relative_paths_debug_provenance_and_source_drift(self) -> None:
        mutations = [
            lambda: self.metadata.update(profile="debug"),
            lambda: self.metadata["variants"]["embedded"].update(source_sha="f" * 40),
            lambda: self.metadata.update(toolchain=""),
            lambda: self.metadata["variants"]["reader"].update(features="implicit"),
        ]
        initial = copy.deepcopy(self.metadata)
        for mutate in mutations:
            self.metadata = copy.deepcopy(initial)
            mutate()
            self.write_metadata()
            self.output.unlink(missing_ok=True)
            with patch.object(benchmark, "invoke") as invoke:
                self.assertEqual(benchmark.main(self.args), 1)
                invoke.assert_not_called()
        self.output.unlink()
        with patch.object(benchmark, "invoke") as invoke:
            self.assertEqual(benchmark.main([*self.args, "--queries", "relative.json"]), 1)
            invoke.assert_not_called()

    def test_every_variant_must_have_the_same_requested_source(self) -> None:
        for name in self.targets:
            with self.subTest(name=name):
                self.metadata["variants"][name]["source_sha"] = "f" * 40
                self.write_metadata()
                self.output.unlink(missing_ok=True)
                with patch.object(benchmark, "invoke") as invoke:
                    self.assertEqual(benchmark.main(self.args), 1)
                    invoke.assert_not_called()
                receipt = json.loads(self.output.read_text(encoding="utf-8"))
                self.assertIn("source SHA", receipt["error"])
                self.assertNotIn("executable_byte_deltas", receipt)
                self.metadata["variants"][name]["source_sha"] = "a" * 40

    def test_declared_features_must_identify_the_three_cli_variants(self) -> None:
        for name in ("baseline", "engine_only", "embedded"):
            initial = self.metadata["variants"][name]["features"]
            self.metadata["variants"][name]["features"] = (
                ["docs-search"] if name == "baseline" else []
            )
            self.write_metadata()
            self.output.unlink(missing_ok=True)
            with self.subTest(name=name), patch.object(benchmark, "invoke") as invoke:
                self.assertEqual(benchmark.main(self.args), 1)
                invoke.assert_not_called()
            receipt = json.loads(self.output.read_text(encoding="utf-8"))
            self.assertIn("Declared features", receipt["error"])
            self.assertNotIn("executable_byte_deltas", receipt)
            self.metadata["variants"][name]["features"] = initial

    def test_unrelated_features_cannot_confound_the_matched_comparison(self) -> None:
        for name in ("baseline", "engine_only", "embedded"):
            initial = self.metadata["variants"][name]["features"]
            self.metadata["variants"][name]["features"] = [*initial, "headless"]
            self.write_metadata()
            self.output.unlink(missing_ok=True)
            with self.subTest(name=name), patch.object(benchmark, "invoke") as invoke:
                self.assertEqual(benchmark.main(self.args), 1)
                invoke.assert_not_called()
            receipt = json.loads(self.output.read_text(encoding="utf-8"))
            self.assertIn("differ only", receipt["error"])
            self.assertNotIn("executable_byte_deltas", receipt)
            self.metadata["variants"][name]["features"] = initial

    def test_identical_executable_hashes_cannot_claim_incompatible_variants(self) -> None:
        for first, second in (
            ("baseline", "engine_only"), ("baseline", "embedded"),
            ("engine_only", "embedded"),
        ):
            original = self.targets[second].read_bytes()
            self.targets[second].write_bytes(self.targets[first].read_bytes())
            self.metadata["variants"][second]["sha256"] = benchmark.artifact(
                self.targets[second]
            )["sha256"]
            self.write_metadata()
            self.output.unlink(missing_ok=True)
            with self.subTest(first=first, second=second), patch.object(benchmark, "invoke") as invoke:
                self.assertEqual(benchmark.main(self.args), 1)
                invoke.assert_not_called()
            receipt = json.loads(self.output.read_text(encoding="utf-8"))
            self.assertIn("distinct executable hashes", receipt["error"])
            self.assertNotIn("executable_byte_deltas", receipt)
            self.targets[second].write_bytes(original)
            self.metadata["variants"][second]["sha256"] = benchmark.artifact(
                self.targets[second]
            )["sha256"]

    def test_wrong_variant_behavior_cannot_accept_size_deltas(self) -> None:
        original = self.fake_invoke
        failures = (
            ("baseline", "success"), ("engine_only", "success"),
            ("baseline", "wrong_exit"), ("engine_only", "wrong_exit"),
            ("baseline", "stdout"), ("engine_only", "stdout"),
            ("baseline", "wrong_error"), ("engine_only", "wrong_error"),
            ("baseline", "extra_error_field"), ("engine_only", "extra_error_field"),
            ("embedded", "missing_corpus"), ("embedded", "invalid_manifest"),
        )
        for target, failure in failures:
            self.output.unlink(missing_ok=True)

            def invoke(
                executable: Path, arguments: list[str], timeout: float, home: Path
            ) -> tuple[dict[str, Any], bytes]:
                row, raw = original(executable, arguments, timeout, home)
                if executable != self.targets[target] or "--corpus-info" not in arguments:
                    return row, raw
                if failure == "success":
                    row.update(exit_code=0, stderr_bytes=0, stderr_preview="")
                    raw = json.dumps({
                        "status": "succeeded", "source": "embeddedFabricDocs",
                        "data": self.manifest,
                    }).encode()
                elif failure == "wrong_exit":
                    row["exit_code"] = 1 if target == "baseline" else 2
                elif failure == "stdout":
                    raw = b"unexpected success-looking output\n"
                elif failure in ("wrong_error", "extra_error_field"):
                    error = copy.deepcopy(benchmark.VARIANT_ERRORS[target])
                    if failure == "wrong_error":
                        error["error"]["message"] = "unsafe echoed supplied query"
                    else:
                        error["unapproved"] = "field"
                    stderr = json.dumps(error)
                    row.update(stderr_preview=stderr, stderr_bytes=len(stderr.encode()))
                elif failure == "missing_corpus":
                    raw = b""
                    row.update(
                        exit_code=1, stderr_bytes=5, stderr_preview="error",
                    )
                elif failure == "invalid_manifest":
                    manifest = dict(self.manifest, documents=0)
                    raw = json.dumps({
                        "status": "succeeded", "source": "embeddedFabricDocs", "data": manifest,
                    }).encode()
                row["stdout_bytes"] = len(raw)
                row["stdout_sha256"] = hashlib.sha256(raw).hexdigest()
                return row, raw

            with self.subTest(target=target, failure=failure), patch.object(
                benchmark, "invoke", side_effect=invoke
            ):
                self.assertEqual(benchmark.main(self.args), 1)
            receipt = json.loads(self.output.read_text(encoding="utf-8"))
            self.assertEqual(receipt["status"], "failed")
            self.assertNotIn("executable_byte_deltas", receipt)
            self.assertNotIn("variant_states", receipt)
            self.assertFalse(receipt["observations"][-1]["valid"])
            self.assertIn("stdout_preview", receipt["observations"][-1])

    def test_optional_reader_and_ram_flag_require_correct_targets(self) -> None:
        args = self.args.copy()
        index = args.index("--reader-benchmark")
        del args[index:index + 2]
        del self.metadata["variants"]["reader"]
        self.write_metadata()
        with patch.object(benchmark, "invoke", side_effect=self.fake_invoke):
            self.assertEqual(benchmark.main(args), 0)
        self.output.unlink()
        with patch.object(benchmark, "invoke") as invoke:
            self.assertEqual(benchmark.main([*args, "--ram-control"]), 1)
            invoke.assert_not_called()

    def test_bounds_reject_before_execution(self) -> None:
        for flag, value in (
            ("--samples", "0"), ("--samples", "201"), ("--samples", "1.5"),
            ("--iterations", "0"), ("--iterations", "1001"),
            ("--warmups", "0"), ("--warmups", "11"),
            ("--deadline-seconds", "601"), ("--deadline-seconds", "nan"),
            ("--process-timeout-seconds", "61"), ("--process-timeout-seconds", "inf"),
        ):
            with (
                self.subTest(flag=flag, value=value),
                patch.object(benchmark, "invoke") as invoke,
                patch("sys.stderr"),
                self.assertRaises(SystemExit),
            ):
                benchmark.main([*self.args, flag, value])
            invoke.assert_not_called()
        self.assertFalse(self.output.exists())

    def test_deadline_prevents_child_and_preserves_failed_receipt(self) -> None:
        with (
            patch.object(benchmark.time, "monotonic", side_effect=[0, 601]),
            patch.object(benchmark, "invoke") as invoke,
        ):
            self.assertEqual(benchmark.main(self.args), 1)
            invoke.assert_not_called()
        receipt = json.loads(self.output.read_text(encoding="utf-8"))
        self.assertIn("deadline", receipt["error"])
        self.assertEqual(receipt["observations"], [])

    def test_artifact_changes_during_run_invalidate_summaries(self) -> None:
        original = self.fake_invoke

        def invoke(
            executable: Path, arguments: list[str], timeout: float, home: Path
        ) -> tuple[dict[str, Any], bytes]:
            result = original(executable, arguments, timeout, home)
            if executable == self.targets["reader"]:
                executable.write_bytes(b"changed during measurement")
            return result

        with patch.object(benchmark, "invoke", side_effect=invoke):
            self.assertEqual(benchmark.main(self.args), 1)
        receipt = json.loads(self.output.read_text(encoding="utf-8"))
        self.assertIn("changed", receipt["error"])
        self.assertNotIn("startup_summaries", receipt)

    def test_bad_output_shape_is_attached_to_the_failed_observation(self) -> None:
        original = self.search_result

        def result(case: dict[str, Any]) -> dict[str, Any]:
            value = original(case)
            value.pop("meta")
            return value

        with patch.object(self, "search_result", side_effect=result):
            status, receipt = self.execute()
        self.assertEqual(status, 1)
        row = receipt["observations"][-1]
        self.assertFalse(row["valid"])
        self.assertEqual(row["failure"], "output_validation_failed")
        self.assertIn("output", row)

    def test_cleanup_error_cannot_leave_a_successful_status(self) -> None:
        remove = shutil.rmtree

        def cleanup(path: Path) -> None:
            remove(path)
            raise OSError("synthetic cleanup failure")

        with patch.object(benchmark.shutil, "rmtree", side_effect=cleanup):
            status, receipt = self.execute()
        self.assertEqual(status, 1)
        self.assertEqual(receipt["status"], "failed")
        self.assertEqual(receipt["error"], "OSError")

    def test_invoke_spools_in_workspace_reaps_and_records_actual_wall_units(self) -> None:
        process = unittest.mock.Mock()
        process.wait.return_value = 0
        process.poll.return_value = 0

        def spawn(command: list[str], **kwargs: Any) -> Any:
            self.assertEqual(command, [str(self.targets["baseline"]), "--version"])
            self.assertEqual(kwargs["stdin"], subprocess.DEVNULL)
            self.assertNotIn("shell", kwargs)
            self.assertEqual(kwargs["cwd"], self.directory)
            self.assertEqual(kwargs["env"]["PATH"], "")
            kwargs["stdout"].write(benchmark.VERSION)
            kwargs["stdout"].flush()
            return process

        with (
            patch.object(benchmark.subprocess, "Popen", side_effect=spawn),
            patch.object(benchmark.time, "perf_counter_ns", side_effect=[1000, 2501000]),
        ):
            row, raw = benchmark.invoke(self.targets["baseline"], ["--version"], 1, self.directory)
        self.assertEqual(row["wall_ms"], 2.5)
        self.assertEqual(row["exit_code"], 0)
        self.assertIsNone(row["failure"])
        self.assertEqual(raw, benchmark.VERSION)
        self.assertEqual(row["stdout_sha256"], hashlib.sha256(raw).hexdigest())
        process.wait.assert_called()

    def test_invoke_launch_failure_and_output_budget_are_not_success(self) -> None:
        with patch.object(benchmark.subprocess, "Popen", side_effect=OSError("private detail")):
            row, _ = benchmark.invoke(self.targets["baseline"], ["--help"], 1, self.directory)
        self.assertEqual(row["failure"], "process_start_failed")
        self.assertIsNone(row["exit_code"])
        self.assertNotIn("private detail", json.dumps(row))
        process = unittest.mock.Mock()
        process.wait.return_value = 0
        process.poll.return_value = 0

        def spawn(_: list[str], **kwargs: Any) -> Any:
            kwargs["stdout"].write(b"x" * 65)
            kwargs["stdout"].flush()
            return process

        with (
            patch.object(benchmark, "MAX_OUTPUT_BYTES", 64),
            patch.object(benchmark.subprocess, "Popen", side_effect=spawn),
        ):
            row, _ = benchmark.invoke(self.targets["baseline"], ["--help"], 1, self.directory)
        self.assertEqual(row["failure"], "output_limit")

    def test_invoke_timeout_kills_only_owned_child_and_reaps_it(self) -> None:
        killed = threading.Event()
        process = unittest.mock.Mock()
        process.pid = 123456789
        process.poll.side_effect = lambda: -9 if killed.is_set() else None
        process.wait.side_effect = lambda: (-9 if killed.wait(1) else 0)
        process.kill.side_effect = killed.set
        # Emulate both platform termination paths without signalling any real PID.
        with (
            patch.object(benchmark.subprocess, "Popen", return_value=process),
            patch.object(benchmark.os, "killpg", side_effect=lambda *_: killed.set(), create=True),
        ):
            row, _ = benchmark.invoke(self.targets["baseline"], ["--help"], 0.01, self.directory)
        self.assertTrue(killed.is_set())
        self.assertEqual(row["failure"], "timeout")
        self.assertEqual(row["exit_code"], -9)
        process.wait.assert_called()


if __name__ == "__main__":
    unittest.main()
