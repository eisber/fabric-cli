# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Small offline fixtures for release metadata and isolated process boundaries.

Synthetic bytes and mocked fab output are not release or clean-machine receipts.
The real process tests invoke only the current test interpreter, never Cargo.
"""

from __future__ import annotations

import contextlib
import copy
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

import release_verify as release


class ReleaseTests(unittest.TestCase):
    """Exercise all preview products and fail-closed metadata guards."""

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.bundle = self.root / "bundle"
        self.bundle.mkdir()
        self.expect_path = self.root / "expected.json"
        self.prepare("x86_64-pc-windows-msvc", "default")

    def prepare(self, target: str, variant: str) -> None:
        """Create the exact existing workflow shape using synthetic bytes."""
        for path in self.bundle.iterdir():
            path.unlink()
        runner, operating_system, architecture, _ = release.TARGETS[target]
        native = variant == "native-read"
        self.filename = f"fab-rs-{variant}" + (".exe" if "windows" in target else "")
        binary = b"synthetic fixture; never a release executable"
        rustc = f"rustc 1.99.1 (fixture 2026-01-01)\nhost: {target}\nrelease: 1.99.1"
        cargo = "cargo 1.99.1 (fixture 2026-01-01)"
        commit, run, attempt = "b" * 40, "1234", "1"
        workflow = "eisber/fabric-cli/.github/workflows/rust-preview-build.yml@refs/heads/feature/rust"
        self.expected: dict[str, Any] = {
            "source_commit": commit,
            "workflow_ref": workflow,
            "target": target,
            "variant": variant,
            "toolchain": release.TOOLCHAIN,
            "rustc_sha256": release._sha(rustc.encode()),
            "cargo_sha256": release._sha(cargo.encode()),
            "lockfile_sha256": "c" * 64,
            "manifest_sha256": "0" * 64,
            "binary_sha256": release._sha(binary),
            "binary_bytes": len(binary),
            "run_id": run,
            "run_attempt": attempt,
            "version": "0.1.0",
        }
        self.manifest: dict[str, Any] = {
            "schema_version": 1,
            "artifact_name": f"fab-rs-{commit}-{target}-{variant}-rust-1.99.1-run-{run}-{attempt}",
            "source": {
                "repository": "eisber/fabric-cli",
                "commit": commit,
                "workflow_ref": workflow,
                "workflow_commit": commit,
            },
            "build": {
                "target": target,
                "features": ["native-read"] if native else [],
                "default_features": False,
                "profile": "release",
                "toolchain": "1.99.1",
                "rustc": rustc,
                "cargo": cargo,
                "lockfile_sha256": "c" * 64,
                "runner": runner,
                "os": operating_system,
                "architecture": architecture,
                "image_os": None,
                "image_version": None,
            },
            "binary": {
                "filename": self.filename,
                "sha256": release._sha(binary),
                "bytes": len(binary),
            },
            "capabilities": {
                "variant": variant,
                "bounded_windows_read_host": native,
                "broker_commands_compiled": native,
                "live_service_verified": False,
                "full_python_cli_parity": False,
            },
            "smoke": {
                "environment": "isolated-allowlist",
                "cases": [
                    {
                        "arguments": list(args),
                        "exit_code": 0 if index < 4 else 2,
                        "stdout_sha256": "0" * 64,
                        "stderr_sha256": "0" * 64,
                    }
                    for index, args in enumerate(release.CASES)
                ],
            },
            "limitations": ["synthetic limitation"] * 6,
            "ci": {
                "run_url": f"https://github.com/eisber/fabric-cli/actions/runs/{run}",
                "run_id": run,
                "run_attempt": attempt,
            },
        }
        (self.bundle / self.filename).write_bytes(binary)
        (self.bundle / "LICENSE").write_text(
            "Synthetic license fixture\n", encoding="utf-8"
        )
        (self.bundle / "README.md").write_text(
            "Synthetic guide fixture\n", encoding="utf-8"
        )
        self.seal()

    def seal(self, raw_manifest: bytes | None = None) -> None:
        """Recompute fixture checksums independently of verifier parsing."""
        raw = (
            raw_manifest
            if raw_manifest is not None
            else json.dumps(self.manifest).encode()
        )
        (self.bundle / "manifest.json").write_bytes(raw)
        self.expected["manifest_sha256"] = release._sha(raw)
        names = sorted((self.filename, "manifest.json", "LICENSE", "README.md"))
        (self.bundle / "SHA256SUMS").write_bytes(
            "".join(
                f"{release._sha((self.bundle / name).read_bytes())}  {name}\n"
                for name in names
            ).encode()
        )
        self.expect_path.write_text(json.dumps(self.expected), encoding="utf-8")

    def verify(self) -> dict[str, Any]:
        """Use the same trusted-input loader as the CLI."""
        return release.verify_bundle(
            self.bundle, release.load_expectations(self.expect_path)
        )

    def test_all_four_preview_products(self) -> None:
        for target in release.TARGETS:
            for variant in (
                ("default", "native-read") if "windows" in target else ("default",)
            ):
                with self.subTest(target=target, variant=variant):
                    self.prepare(target, variant)
                    receipt = self.verify()
                    self.assertEqual(receipt["result"], "bundle-verified")
                    self.assertEqual(len(receipt["file_sha256"]), 5)
                    for claim in (
                        "publisher_signature_verified",
                        "clean_machine_verified",
                        "live_service_verified",
                    ):
                        self.assertIs(receipt[claim], False)

    def test_metadata_mutations_even_with_recomputed_checksums(self) -> None:
        mutations = [
            ((), "schema_version", 2),
            ((), "schema_version", True),
            ((), "artifact_name", "moving-name"),
            ((), "limitations", []),
            (("source",), "commit", "main"),
            (("source",), "repository", "other/fork"),
            (("source",), "workflow_commit", "a" * 40),
            (("source",), "workflow_ref", "other-workflow"),
            (("build",), "target", "aarch64-apple-darwin"),
            (("build",), "features", ["windows-broker"]),
            (("build",), "default_features", 0),
            (("build",), "profile", "debug"),
            (("build",), "toolchain", "stable"),
            (("build",), "rustc", "rustc 1.99.1 forged"),
            (("build",), "cargo", "cargo 1.99.1 forged"),
            (("build",), "lockfile_sha256", "d" * 64),
            (("build",), "runner", "windows-latest"),
            (("build",), "os", "Linux"),
            (("build",), "architecture", "ARM64"),
            (("build",), "image_version", 123),
            (("binary",), "filename", "../fab.exe"),
            (("binary",), "filename", "C:\\fab.exe"),
            (("binary",), "sha256", "e" * 64),
            (("binary",), "bytes", 1),
            (("binary",), "bytes", True),
            (("capabilities",), "live_service_verified", True),
            (("capabilities",), "broker_commands_compiled", True),
            (("capabilities",), "full_python_cli_parity", True),
            (("ci",), "run_url", "https://example.invalid"),
            (("ci",), "run_attempt", "2"),
            (("smoke",), "environment", "inherited"),
            (("smoke",), "cases", []),
        ]
        original = copy.deepcopy(self.manifest)
        for path, key, value in mutations:
            with self.subTest(path=path, key=key, value=value):
                self.manifest = copy.deepcopy(original)
                node = self.manifest
                for part in path:
                    node = node[part]
                node[key] = value
                self.seal()
                with self.assertRaises(release.ReleaseError):
                    self.verify()

    def test_bad_expectations(self) -> None:
        for key, value in (
            ("source_commit", "main"),
            ("target", []),
            ("target", "unlisted-target"),
            ("toolchain", "stable"),
            ("variant", "all"),
            ("binary_bytes", True),
            ("binary_bytes", release.MAX_BINARY + 1),
            ("binary_bytes", 0),
            ("binary_sha256", "F" * 64),
            ("run_id", "0"),
            ("run_attempt", 1),
            ("version", "0.1.0\ninjected"),
            ("workflow_ref", "untrusted"),
        ):
            with self.subTest(key=key):
                value_map = dict(self.expected, **{key: value})
                self.expect_path.write_text(json.dumps(value_map), encoding="utf-8")
                with self.assertRaises(release.ReleaseError):
                    release.load_expectations(self.expect_path)

    def test_native_read_not_allowed_on_other_targets(self) -> None:
        self.expected.update(target="aarch64-apple-darwin", variant="native-read")
        self.seal()
        with self.assertRaisesRegex(release.ReleaseError, "requires Windows"):
            release.load_expectations(self.expect_path)

    def test_exact_allowlist_and_regular_files(self) -> None:
        extra = self.bundle / "ambient-token.txt"
        extra.write_text("synthetic", encoding="utf-8")
        with self.assertRaisesRegex(release.ReleaseError, "allowlist"):
            self.verify()
        extra.unlink()
        guide = self.bundle / "README.md"
        guide.unlink()
        with self.assertRaisesRegex(release.ReleaseError, "allowlist"):
            self.verify()
        guide.mkdir()
        with self.assertRaisesRegex(release.ReleaseError, "regular file"):
            self.verify()

    def test_hardlink_rejected(self) -> None:
        source = self.root / "linked"
        source.write_bytes((self.bundle / self.filename).read_bytes())
        (self.bundle / self.filename).unlink()
        os.link(source, self.bundle / self.filename)
        with self.assertRaisesRegex(release.ReleaseError, "Hard-linked"):
            self.verify()

    def test_symlink_rejected_when_host_permits_creation(self) -> None:
        source = self.root / "outside"
        source.write_text("outside", encoding="utf-8")
        link = self.root / "link"
        try:
            link.symlink_to(source)
        except OSError as error:
            self.skipTest(f"Host cannot create symlinks (errno {error.errno})")
        (self.bundle / "README.md").unlink()
        link.rename(self.bundle / "README.md")
        with self.assertRaisesRegex(release.ReleaseError, "Links"):
            self.verify()

    def test_tampering_and_untrusted_checksum_paths(self) -> None:
        original = (self.bundle / "SHA256SUMS").read_bytes()
        for raw in (
            original + original,
            original.replace(b"  LICENSE", b"  ../LICENSE"),
            original.upper(),
            original.replace(b"\n", b"\r\n"),
            original.replace(b"  README.md", b"  C:\\README.md"),
            b"",
        ):
            with self.subTest(raw=raw[:20]):
                (self.bundle / "SHA256SUMS").write_bytes(raw)
                with self.assertRaises(release.ReleaseError):
                    self.verify()
        (self.bundle / "SHA256SUMS").write_bytes(original)
        (self.bundle / self.filename).write_bytes(b"tampered")
        with self.assertRaises(release.ReleaseError):
            self.verify()

    def test_manifest_digest_is_independently_pinned(self) -> None:
        self.manifest["limitations"].append("changed after trusted receipt")
        pinned = self.expected["manifest_sha256"]
        self.seal()
        self.expected["manifest_sha256"] = pinned
        self.expect_path.write_text(json.dumps(self.expected), encoding="utf-8")
        with self.assertRaisesRegex(release.ReleaseError, "Manifest digest"):
            self.verify()

    def test_invalid_duplicate_and_unknown_json(self) -> None:
        for raw in (
            b"{",
            b"\xff",
            b'{"a":1,"a":2}',
            b'{"unknown": true}',
            json.dumps(self.manifest).encode("utf-16"),
            b'{"huge":' + b"1" * 5000 + b"}",
        ):
            with self.subTest(raw=raw):
                self.seal(raw)
                with self.assertRaises(release.ReleaseError):
                    self.verify()
        raw = (
            json.dumps(self.manifest)
            .encode()
            .replace(
                b'"schema_version": 1', b'"schema_version": 1, "schema_version": 1'
            )
        )
        self.seal(raw)
        with self.assertRaisesRegex(release.ReleaseError, "Duplicate"):
            self.verify()

    def test_compiler_encoding_failure_is_sanitized(self) -> None:
        self.manifest["build"]["rustc"] = "\ud800"
        self.seal()
        with self.assertRaisesRegex(release.ReleaseError, "compiler identity encoding"):
            self.verify()

    def test_metadata_and_binary_size_bounds(self) -> None:
        with patch.object(release, "MAX_METADATA", 16):
            with self.assertRaisesRegex(release.ReleaseError, "size limit"):
                self.verify()
        with patch.object(release, "MAX_BINARY", 8):
            with self.assertRaisesRegex(release.ReleaseError, "length pin"):
                self.verify()

    def test_recorded_cases_require_exact_argv_exit_and_hash(self) -> None:
        original = copy.deepcopy(self.manifest)
        for key, value in (
            ("arguments", ["auth", "login"]),
            ("exit_code", True),
            ("stdout_sha256", "bad"),
        ):
            self.manifest = copy.deepcopy(original)
            self.manifest["smoke"]["cases"][0][key] = value
            self.seal()
            with self.assertRaisesRegex(release.ReleaseError, "recorded smoke"):
                self.verify()

    def test_absolute_paths_required(self) -> None:
        with self.assertRaisesRegex(release.ReleaseError, "absolute"):
            release.load_expectations(Path("expected.json"))
        with self.assertRaisesRegex(release.ReleaseError, "absolute"):
            release.verify_bundle(Path("bundle"), self.expected)

    def fake_run(
        self,
        executable: Path,
        args: tuple[str, ...],
        sandbox: Path,
        environment: dict[str, str],
        timeout: float,
    ) -> tuple[int, bytes, bytes]:
        """Check copy/isolation, then return synthetic existing CLI contracts."""
        self.assertNotEqual(executable.parent, self.bundle)
        self.assertTrue(executable.is_absolute())
        self.assertEqual(
            executable.read_bytes(), (self.bundle / self.filename).read_bytes()
        )
        self.assertEqual(environment["PATH"], "")
        self.assertFalse(
            any(
                key in environment
                for key in (
                    "FAB_TOKEN",
                    "AZURE_CLIENT_SECRET",
                    "HTTP_PROXY",
                    "PYTHONPATH",
                    "CARGO_HOME",
                )
            )
        )
        self.assertNotEqual(sandbox, executable.parent)
        self.assertEqual(timeout, 10)
        if args and args[0] in ("--help", "-h"):
            return 0, b"Usage: fab-rs\n--help --version\n", b""
        if args and args[0] in ("--version", "-V"):
            return 0, b"fab-rs 0.1.0\n", b""
        return 2, b"", json.dumps(release.INVALID).encode() + b"\n"

    def test_full_mocked_smoke_uses_copy_scrubs_environment_and_cleans_up(self) -> None:
        paths: list[Path] = []

        def capture(
            executable: Path,
            args: tuple[str, ...],
            sandbox: Path,
            environment: dict[str, str],
            timeout: float,
        ) -> tuple[int, bytes, bytes]:
            paths.append(executable)
            return self.fake_run(executable, args, sandbox, environment, timeout)

        with (
            patch.dict(os.environ, {"FAB_TOKEN": "synthetic", "PYTHONPATH": "trap"}),
            patch.object(release, "_native_host", return_value=True),
            patch.object(release, "_run", side_effect=capture) as run,
        ):
            receipt = release.smoke_bundle(self.bundle, self.expected)
        self.assertEqual(run.call_count, 8)
        self.assertEqual(receipt["result"], "copied-executable-smoke-passed")
        self.assertEqual(len(receipt["smoke"]["cases"]), 8)
        self.assertFalse(receipt["clean_machine_verified"])
        self.assertTrue(all(not path.exists() for path in paths))

    def test_smoke_wrong_host_cannot_execute(self) -> None:
        with (
            patch.object(release, "_native_host", return_value=False),
            patch.object(release, "_run") as run,
        ):
            with self.assertRaisesRegex(release.ReleaseError, "matching native"):
                release.smoke_bundle(self.bundle, self.expected)
            run.assert_not_called()

    def test_native_host_matching(self) -> None:
        for target, system, machine, valid in (
            ("x86_64-pc-windows-msvc", "Windows", "AMD64", True),
            ("x86_64-pc-windows-msvc", "Windows", "ARM64", False),
            ("x86_64-pc-windows-msvc", "Linux", "x86_64", False),
            ("x86_64-unknown-linux-gnu", "Linux", "x86_64", True),
            ("aarch64-apple-darwin", "Darwin", "arm64", True),
            ("aarch64-apple-darwin", "Darwin", "x86_64", False),
        ):
            with (
                self.subTest(target=target, system=system, machine=machine),
                patch.object(release.platform, "system", return_value=system),
                patch.object(release.platform, "machine", return_value=machine),
            ):
                self.assertEqual(release._native_host(target), valid)

    def test_smoke_timeout_configuration_is_bounded(self) -> None:
        for timeout in (0, -1, 11, float("nan")):
            with self.subTest(timeout=timeout), self.assertRaises(release.ReleaseError):
                release.smoke_bundle(self.bundle, self.expected, timeout)

    def test_version_error_and_redaction_failures(self) -> None:
        for bad_index, result in (
            (2, (0, b"fab-rs wrong-version\n", b"")),
            (2, (0, b"fab-rs 0.1.0\r\n", b"")),
            (4, (2, b"unexpected stdout", b"{}\n")),
            (4, (2, b"", b'{"error": "must-not-be-echoed"}\n')),
            (4, (2, b"", b'{"error": "wrong envelope"}\n')),
            (4, (2, b"", b"not-json\n")),
        ):
            calls = 0

            def failing_run(
                executable: Path,
                args: tuple[str, ...],
                sandbox: Path,
                environment: dict[str, str],
                timeout: float,
            ) -> tuple[int, bytes, bytes]:
                nonlocal calls
                index = calls
                calls += 1
                if index == bad_index:
                    return result
                return self.fake_run(executable, args, sandbox, environment, timeout)

            with (
                self.subTest(index=bad_index, result=result),
                patch.object(release, "_native_host", return_value=True),
                patch.object(release, "_run", side_effect=failing_run),
            ):
                with self.assertRaises(release.ReleaseError) as error:
                    release.smoke_bundle(self.bundle, self.expected)
                self.assertNotIn("must-not-be-echoed", str(error.exception))

    def test_smoke_detects_copy_or_original_mutation(self) -> None:
        for mutate_original in (False, True):
            self.prepare("x86_64-pc-windows-msvc", "default")
            copy_paths: list[Path] = []

            def mutate(
                executable: Path,
                args: tuple[str, ...],
                sandbox: Path,
                environment: dict[str, str],
                timeout: float,
            ) -> tuple[int, bytes, bytes]:
                result = self.fake_run(executable, args, sandbox, environment, timeout)
                copy_paths.append(executable)
                if args == release.CASES[-1]:
                    path = (
                        self.bundle / self.filename if mutate_original else executable
                    )
                    path.write_bytes(b"changed")
                return result

            with (
                self.subTest(original=mutate_original),
                patch.object(release, "_native_host", return_value=True),
                patch.object(release, "_run", side_effect=mutate),
            ):
                with self.assertRaises(release.ReleaseError):
                    release.smoke_bundle(self.bundle, self.expected)
            self.assertTrue(all(not path.exists() for path in copy_paths))

    def test_missing_executable_reports_no_native_error_text(self) -> None:
        work = self.root / "work"
        work.mkdir()
        with self.assertRaisesRegex(
            release.ReleaseError, "^Copied executable could not start$"
        ):
            release._run(
                self.root / "missing.exe", (), work, release._environment(work), 1
            )

    def test_smoke_contract_failures_do_not_print_child_output(self) -> None:
        for result in (
            (1, b"", b"synthetic-secret"),
            (0, b"wrong help", b""),
            (0, b"Usage: fab-rs --help --version", b"synthetic-secret"),
        ):
            with (
                self.subTest(result=result),
                patch.object(release, "_native_host", return_value=True),
                patch.object(release, "_run", return_value=result),
            ):
                with self.assertRaises(release.ReleaseError) as error:
                    release.smoke_bundle(self.bundle, self.expected)
                self.assertNotIn("synthetic-secret", str(error.exception))

    def test_cli_failure_is_sanitized_and_nonzero(self) -> None:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = release.main(["--unknown=synthetic-secret"])
        self.assertEqual(code, 1)
        self.assertEqual(out.getvalue(), "")
        self.assertNotIn("synthetic-secret", err.getvalue())
        self.assertEqual(
            json.loads(err.getvalue())["error"]["code"], "release_verification_failed"
        )

    def test_cli_verify_outputs_actual_receipt_without_execution(self) -> None:
        out = io.StringIO()
        with contextlib.redirect_stdout(out), patch.object(release, "_run") as run:
            code = release.main(
                ["--bundle", str(self.bundle), "--expect", str(self.expect_path)]
            )
        self.assertEqual(code, 0)
        self.assertEqual(
            json.loads(out.getvalue())["file_sha256"][self.filename],
            self.expected["binary_sha256"],
        )
        run.assert_not_called()

    def test_real_process_environment_timeout_and_output_limits(self) -> None:
        sandbox = self.root / "work"
        sandbox.mkdir()
        environment = release._environment(sandbox)
        executable = Path(sys.executable).resolve()
        code, out, err = release._run(
            executable,
            (
                "-I",
                "-S",
                "-c",
                "import os; print(os.environ.get('PATH')); print(os.environ.get('FAB_TOKEN', 'absent'))",
            ),
            sandbox,
            environment,
            5,
        )
        self.assertEqual(code, 0)
        self.assertEqual(out.replace(b"\r\n", b"\n"), b"\nabsent\n")
        self.assertEqual(err, b"")
        with self.assertRaisesRegex(release.ReleaseError, "timed out"):
            release._run(
                executable,
                ("-I", "-S", "-c", "import time; time.sleep(60)"),
                sandbox,
                environment,
                0.05,
            )
        with patch.object(release, "MAX_OUTPUT", 1024):
            with self.assertRaisesRegex(release.ReleaseError, "output exceeded"):
                release._run(
                    executable,
                    ("-I", "-S", "-c", "import sys; sys.stdout.write('x' * 4096)"),
                    sandbox,
                    environment,
                    5,
                )


if __name__ == "__main__":
    unittest.main()
