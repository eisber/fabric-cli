# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Offline local-Git tests for the isolated historical Python source provider."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import pinned_python_source as provider


class PinnedPythonSourceTests(unittest.TestCase):
    """Keep provenance guards intact while candidate product source advances."""

    def setUp(self) -> None:
        """Create only tiny synthetic repositories under project artifacts."""
        artifacts = (
            Path(__file__).resolve().parents[1]
            / "target"
            / "pinned-python-source-tests"
        )
        artifacts.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(dir=artifacts)
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name).resolve()
        self.candidate = self.root / "candidate"
        self.candidate.mkdir()
        self.git("init", "--quiet")
        self.write("src/fabric_cli/example.py", b"VALUE = 'frozen'\n")
        self.write("src/fabric_cli/payloads/template.json", b'{"value": "frozen"}\n')
        self.pin = self.commit("historical Python")
        self.write("src/fabric_cli/example.py", b"VALUE = 'new main'\n")
        self.write("rust/Cargo.toml", b"[workspace]\n")
        self.write(
            "rust/cli/capability/tests/python_reference.py",
            b"# candidate reference, not historical implementation\r\n",
        )
        self.write(
            "rust/cli/capability/Cargo.toml",
            b'[package]\nname = "example"\n',
        )
        self.write("rust/tests/compatibility/example/cases.json", b'{"frozen": true}\n')
        self.write("rust/tests/compatibility/example/reference.py", b"VALUE = 1\n")
        self.write("rust/scripts/other_helper.py", b"VALUE = 2\n")
        self.write("rust/cli/capability/src/lib.rs", b"// Never built in the oracle.\n")
        self.template = (
            "rust/capabilities/resources/fabric-cli-mutations/"
            "templates/Blank.SemanticModel/.platform"
        )
        self.evidence = (
            "rust/crates/fabric-cli-mutations/templates/Blank.SemanticModel/.platform"
        )
        self.write(self.template, b'{"candidate_native_template": true}\n')
        self.write(
            "rust/capabilities/resources/fabric-cli-mutations/src/templates.rs",
            b"// Product implementation is not oracle evidence.\n",
        )
        self.head = self.commit("candidate Rust and advanced Python", self.pin)
        self.archive = (
            "rust/tests/compatibility/workspace_get/python-source-archive.json"
        )
        self.write(self.archive, b'{"untracked_candidate_archive": true}\n')
        self.write(".gitignore", b"rust/target/\n")
        self.write("rust/target/debug/probe.exe", b"not an oracle input")
        self.output = self.root / "artifacts"
        pin_patch = patch.object(provider, "SOURCE_COMMIT", self.pin)
        self.addCleanup(pin_patch.stop)
        pin_patch.start()

    def git(self, *arguments: str, data: bytes | None = None) -> bytes:
        """Run local Git without ambient repository selectors or identity."""
        environment = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith("GIT_")
        }
        environment.update(
            GIT_AUTHOR_NAME="Offline test",
            GIT_AUTHOR_EMAIL="offline@example.invalid",
            GIT_COMMITTER_NAME="Offline test",
            GIT_COMMITTER_EMAIL="offline@example.invalid",
        )
        return subprocess.check_output(
            [
                "git",
                "-c",
                "core.autocrlf=false",
                "-c",
                "commit.gpgsign=false",
                *arguments,
            ],
            cwd=self.candidate,
            input=data,
            env=environment,
            stderr=subprocess.PIPE,
            timeout=30,
        )

    def write(self, relative: str, contents: bytes) -> None:
        """Write one synthetic fixture without touching the actual repository."""
        path = self.candidate.joinpath(*relative.split("/"))
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(contents)

    def commit(self, message: str, parent: str | None = None) -> str:
        """Create synthetic Git objects without hooks or a product commit."""
        self.git("add", "--all")
        tree = self.git("write-tree").decode().strip()
        parents = ["-p", parent] if parent else []
        commit = (
            self.git("commit-tree", tree, *parents, data=message.encode())
            .decode()
            .strip()
        )
        self.git("update-ref", "HEAD", commit)
        return commit

    def prepare(self) -> Path:
        """Prepare the artifact and return its isolated checkout."""
        receipt = provider.prepare(self.candidate, self.output, self.pin)
        return Path(receipt["checkout"])

    def test_isolated_pin_and_byte_identical_candidate_oracles(self) -> None:
        """Copy advanced candidate harnesses, not advanced Python or Rust code."""
        before = self.git("status", "--porcelain")
        checkout = self.prepare()
        self.assertEqual(self.git("rev-parse", "HEAD").decode().strip(), self.head)
        self.assertEqual(self.git("status", "--porcelain"), before)
        self.assertEqual(
            (checkout / "src/fabric_cli/example.py").read_bytes(),
            b"VALUE = 'frozen'\n",
        )
        self.assertEqual(
            (self.candidate / "src/fabric_cli/example.py").read_bytes(),
            b"VALUE = 'new main'\n",
        )
        for value in provider.overlay_paths(self.candidate):
            self.assertEqual(
                (checkout / value).read_bytes(), (self.candidate / value).read_bytes()
            )
        self.assertTrue((checkout / self.archive).is_file())
        self.assertFalse((checkout / "rust/cli/capability/src/lib.rs").exists())
        self.assertFalse(
            (
                checkout
                / "rust/capabilities/resources/fabric-cli-mutations/src/templates.rs"
            ).exists()
        )
        self.assertFalse((checkout / "rust/target").exists())
        receipt = provider.verify(self.output)
        self.assertEqual(receipt["source_commit"], self.pin)
        self.assertEqual(receipt["candidate_commit"], self.head)
        self.assertEqual(receipt["evidence_file_count"], 1)

    def test_native_template_evidence_retains_candidate_custody(self) -> None:
        """Historical lookup paths alias candidate inputs, never frozen exports."""
        checkout = self.prepare()
        raw = (self.candidate / self.template).read_bytes()
        self.assertEqual((checkout / self.template).read_bytes(), raw)
        self.assertEqual((checkout / self.evidence).read_bytes(), raw)
        manifest = json.loads(
            (self.output / "manifest.json").read_text(encoding="utf-8")
        )
        self.assertEqual(
            manifest["overlays"][self.evidence],
            {"source": self.template, "sha256": provider._digest(raw)},
        )

    def test_altered_native_evidence_and_claimed_source_are_rejected(self) -> None:
        """Even a rewritten receipt cannot detach evidence from candidate bytes."""
        checkout = self.prepare()
        evidence = checkout / self.evidence
        evidence.write_bytes(b"substituted native template\n")
        manifest_path = self.output / "manifest.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["overlays"][self.evidence] = {
            "source": self.archive,
            "sha256": provider._digest(evidence.read_bytes()),
        }
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "Oracle overlay changed"):
            provider.verify(self.output)

    def test_missing_native_evidence_is_rejected(self) -> None:
        """A missing alias is a changed inventory, not a skipped native check."""
        checkout = self.prepare()
        (checkout / self.evidence).unlink()
        with self.assertRaisesRegex(ValueError, "overlay inventory"):
            provider.verify(self.output)

    def test_conflicting_candidate_evidence_is_rejected(self) -> None:
        """Do not silently overwrite a real candidate file at an alias location."""
        self.write(self.evidence, b"conflicting legacy template\n")
        with self.assertRaisesRegex(ValueError, "conflicts with oracle evidence"):
            provider.prepare(self.candidate, self.output, self.pin)
        self.assertFalse(self.output.exists())

    def test_wrong_pin_is_rejected_without_creating_output(self) -> None:
        """Never turn an upstream synchronization into an oracle repin."""
        with self.assertRaisesRegex(ValueError, "Only the frozen"):
            provider.prepare(self.candidate, self.output, self.head)
        self.assertFalse(self.output.exists())

    def test_missing_pin_is_rejected_without_current_source_fallback(self) -> None:
        """An absent historical object is a blocker rather than current main."""
        with patch.object(provider, "SOURCE_COMMIT", "1" * 40):
            with self.assertRaisesRegex(ValueError, "unavailable locally"):
                provider.prepare(self.candidate, self.output, "1" * 40)
        self.assertFalse(self.output.exists())

    def test_modified_historical_source_is_rejected(self) -> None:
        """Retain source-byte checking even when Git HEAD remains historical."""
        checkout = self.prepare()
        (checkout / "src/fabric_cli/example.py").write_bytes(b"changed\n")
        with self.assertRaisesRegex(ValueError, "Historical Python source changed"):
            provider.verify(self.output)

    def test_additional_historical_source_is_rejected(self) -> None:
        """Extra importable files cannot escape the complete source inventory."""
        checkout = self.prepare()
        (checkout / "src/fabric_cli/unreviewed.py").write_bytes(b"changed\n")
        with self.assertRaisesRegex(ValueError, "source file inventory"):
            provider.verify(self.output)

    def test_altered_overlay_and_altered_manifest_are_rejected(self) -> None:
        """Self-declared replacement hashes cannot bless changed expectations."""
        checkout = self.prepare()
        value = "rust/tests/compatibility/example/cases.json"
        (checkout / value).write_bytes(b'{"weakened": true}\n')
        manifest_path = self.output / "manifest.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["overlays"][value]["sha256"] = provider._digest(
            (checkout / value).read_bytes()
        )
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "Oracle overlay changed"):
            provider.verify(self.output)

    def test_missing_and_additional_overlays_are_rejected(self) -> None:
        """Neither fixture removal nor an extra replacement helper is permitted."""
        checkout = self.prepare()
        extra = checkout / "rust/scripts/replacement.py"
        extra.write_bytes(b"unreviewed\n")
        with self.assertRaisesRegex(ValueError, "overlay inventory"):
            provider.verify(self.output)
        extra.unlink()
        (checkout / self.archive).unlink()
        with self.assertRaisesRegex(ValueError, "overlay inventory"):
            provider.verify(self.output)

    def test_candidate_changes_are_rejected(self) -> None:
        """Bind copied expectations to the candidate inventory and exact bytes."""
        self.prepare()
        self.write(self.archive, b'{"changed": true}\n')
        with self.assertRaisesRegex(ValueError, "Oracle overlay changed"):
            provider.verify(self.output)

    def test_current_head_fallback_is_rejected(self) -> None:
        """Even restoring historical src bytes does not excuse a current HEAD."""
        checkout = self.prepare()
        provider._git(checkout, "update-ref", "--no-deref", "HEAD", self.head)
        with self.assertRaisesRegex(ValueError, "HEAD differs"):
            provider.verify(self.output)

    def test_reuse_never_overwrites_existing_artifacts(self) -> None:
        """Leave prior artifacts intact instead of recursive cleanup or resets."""
        self.prepare()
        marker = self.output / "retained.txt"
        marker.write_bytes(b"keep")
        with self.assertRaises(FileExistsError):
            provider.prepare(self.candidate, self.output, self.pin)
        self.assertEqual(marker.read_bytes(), b"keep")

    def test_relative_paths_and_overlay_output_are_rejected(self) -> None:
        """Require deliberate artifact coordinates outside oracle inputs."""
        with self.assertRaisesRegex(ValueError, "must be absolute"):
            provider.prepare(self.candidate, Path("relative"), self.pin)
        with self.assertRaisesRegex(ValueError, "inside an oracle"):
            provider.prepare(
                self.candidate, self.candidate / "rust/scripts/output", self.pin
            )

    def test_unsafe_manifest_path_is_rejected(self) -> None:
        """Do not resolve traversal entries from an edited artifact manifest."""
        self.prepare()
        manifest_path = self.output / "manifest.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["overlays"]["../outside"] = "0" * 64
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "overlay inventory"):
            provider.verify(self.output)


class PreparedResourceTemplateTests(unittest.TestCase):
    """Exercise the unchanged resource oracle in a real prepared checkout."""

    def test_prepared_snapshot_executes_real_template_validation_contract(self) -> None:
        """Check candidate native evidence, not just synthetic overlay inventory."""
        candidate = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory(prefix="fab-pin-") as directory:
            output = Path(directory).resolve() / "prepared"
            receipt = provider.prepare(candidate, output)
            checkout = Path(receipt["checkout"])
            references = (
                checkout
                / "rust"
                / "capabilities"
                / "resources"
                / "fabric-cli-mutations"
                / "tests"
            )

            def validate_templates() -> subprocess.CompletedProcess[str]:
                return subprocess.run(
                    [
                        sys.executable,
                        "-I",
                        "-B",
                        "-c",
                        "import sys; from pathlib import Path; "
                        "sys.path.insert(0, sys.argv[2]); "
                        "from creation_reference import verify_templates; "
                        "verify_templates(Path(sys.argv[1]))",
                        str(checkout),
                        str(references),
                    ],
                    cwd=checkout,
                    capture_output=True,
                    text=True,
                    encoding="utf-8",
                    timeout=120,
                    check=False,
                )

            result = validate_templates()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertGreater(receipt["evidence_file_count"], 0)
            manifest = json.loads(
                (output / "manifest.json").read_text(encoding="utf-8")
            )
            evidence = {
                checkout / value: (checkout / value).read_bytes()
                for value, record in manifest["overlays"].items()
                if value != record["source"]
            }
            template = next(iter(evidence))
            template.write_bytes(b"not the candidate's native template\n")
            result = validate_templates()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Native embedded template differs from pin", result.stderr)
            with self.assertRaisesRegex(ValueError, "Oracle overlay changed"):
                provider.verify(output)
            for path in evidence:
                path.unlink()
            result = validate_templates()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("No native templates were checked", result.stderr)
            with self.assertRaisesRegex(ValueError, "overlay inventory"):
                provider.verify(output)
            for path, raw in evidence.items():
                path.write_bytes(raw)
            provider.verify(output)


if __name__ == "__main__":
    unittest.main()
