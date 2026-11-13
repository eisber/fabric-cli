# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Synthetic evidence contracts only; no product execution or measured performance."""

import copy
import json
from pathlib import Path
import tempfile
import unittest

import docs_space_impact as impact


class SpaceImpactTests(unittest.TestCase):
    """Prevent estimates, unmatched profiles, and failed builds becoming accepted impacts."""

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.request = {"schema_version": 1, "profiles": {}}
        for ordinal, (name, features) in enumerate(impact.PROFILES.items()):
            directory = self.root / name
            directory.mkdir()
            component = directory / "meta.json"
            component.write_bytes(b"synthetic component" + b"x" * ordinal)
            manifest = {
                "schema_version": 1, "schema_id": "fabric-docs-sections-v2",
                "tantivy_version": "0.27.0", "source_commit": "a" * 40,
                "source_repository": "https://example.invalid/synthetic",
                "source_markdown_files": 2, "source_markdown_bytes": 100,
                "documents": 3, "compression": "lz4", "segments": 1,
                "unresolved_includes": [], "skipped_inputs": [],
                "features": features, "files": [{
                    "name": component.name, "bytes": component.stat().st_size,
                    "sha256": impact.digest(component),
                }],
            }
            manifest_path = directory / "bundle.json"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            binary = directory / "synthetic.bin"
            binary.write_bytes(b"not an executable" + b"x" * (2 * ordinal))
            receipt = directory / "build.json"
            receipt.write_text(json.dumps({
                "exit": 0, "stopped": None, "source_unchanged": True,
                "source_before": "b" * 64, "source_after": "b" * 64,
                "embedded_index": str(directory),
                "argv": ["synthetic-indexer-not-executed", "--profile", name, "--output", str(directory)],
            }), encoding="utf-8")
            probe = directory / "corpus-probe.json"
            probe.write_text(json.dumps({
                "exit": 0, "stderr": "", "executable_sha256": impact.digest(binary),
                "response": {"status": "succeeded", "source": "embeddedFabricDocs", "data": manifest},
            }), encoding="utf-8")
            provenance = directory / "provenance.json"
            provenance.write_text(json.dumps({
                "source_sha": "a" * 40, "source_digest": "b" * 64,
                "lockfile_sha256": "c" * 64, "target_triple": "synthetic-not-native",
                "features": ["docs-search"], "default_features": False,
                "compiler": "synthetic fixture", "profile": "release",
                "path": str(binary), "bytes": binary.stat().st_size,
                "sha256": impact.digest(binary),
                "index_manifest_sha256": impact.digest(manifest_path),
                "build_receipt": str(receipt), "build_receipt_sha256": impact.digest(receipt),
                "corpus_probe": str(probe), "corpus_probe_sha256": impact.digest(probe),
            }), encoding="utf-8")
            self.request["profiles"][name] = {
                "bundle": str(directory), "provenance": str(provenance),
            }

    def rewrite(self, path: Path, key: str, value: object) -> None:
        """Mutate one visibly synthetic evidence field."""
        data = impact.read(path)
        data[key] = value
        path.write_text(json.dumps(data), encoding="utf-8")

    def test_isolated_and_combined_deltas_use_one_baseline(self) -> None:
        result = impact.measure(self.request)
        self.assertEqual(result["status"], "passed")
        self.assertEqual(len(result["impact"]), 6)
        self.assertEqual(result["impact"][0]["additional_executable_bytes"], 0)
        self.assertEqual(result["impact"][-1]["additional_index_bytes"], 5)
        self.assertEqual(result["impact"][-1]["additional_executable_bytes"], 10)

    def test_missing_profile_cannot_be_substituted_by_an_estimate(self) -> None:
        request = copy.deepcopy(self.request)
        request["profiles"].pop("stemmed")
        with self.assertRaises(ValueError):
            impact.measure(request)

    def test_explicit_payload_metric_never_claims_executable_deltas(self) -> None:
        request = copy.deepcopy(self.request)
        request["mode"] = "embedded-payload"
        for name, entry in request["profiles"].items():
            entry["index_receipt"] = str(self.root / name / "build.json")
        result = impact.measure_payload(request)
        self.assertEqual(result["status"], "passed")
        self.assertEqual(result["metric"], "embedded-index-plus-manifest-bytes")
        self.assertIn("Not measured per feature", result["executable_delta"])
        self.assertNotIn("executable_bytes", result["impact"][0])
        self.assertEqual(result["impact"][0]["additional_embedded_bytes"], 0)
        self.rewrite(self.root / "stemmed" / "build.json", "exit", 101)
        with self.assertRaises(ValueError):
            impact.measure_payload(request)

    def test_executable_probe_must_expose_the_actual_bundle(self) -> None:
        folder = self.root / "stemmed"
        probe = impact.read(folder / "corpus-probe.json")
        probe["response"]["data"]["features"] = impact.PROFILES["baseline"]
        (folder / "corpus-probe.json").write_text(json.dumps(probe), encoding="utf-8")
        provenance = impact.read(folder / "provenance.json")
        provenance["corpus_probe_sha256"] = impact.digest(folder / "corpus-probe.json")
        (folder / "provenance.json").write_text(json.dumps(provenance), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "does not expose"):
            impact.measure(self.request)

    def test_wrong_profile_failed_or_changed_receipts_are_not_impacts(self) -> None:
        for kind in ("features", "exit", "source", "component", "binary", "corpus"):
            with self.subTest(kind=kind):
                self.setUp()
                folder = self.root / "stemmed"
                if kind == "features":
                    self.rewrite(folder / "bundle.json", "features", impact.PROFILES["full"])
                elif kind == "exit":
                    self.rewrite(folder / "build.json", "exit", 101)
                elif kind == "source":
                    self.rewrite(folder / "build.json", "source_after", "d" * 64)
                elif kind == "component":
                    (folder / "meta.json").write_bytes(b"tampered")
                elif kind == "binary":
                    (folder / "synthetic.bin").write_bytes(b"tampered")
                else:
                    self.rewrite(folder / "bundle.json", "documents", 4)
                with self.assertRaises(ValueError):
                    impact.measure(self.request)


if __name__ == "__main__":
    unittest.main()
