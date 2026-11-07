# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Offline tests of the engineering harness, never native-product acceptance."""

from __future__ import annotations

import copy
import json
import os
import platform
import shutil
import struct
import sys
import time
import unittest
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest.mock import patch

import native_acceptance as acceptance


FIXTURES = Path(__file__).resolve().parents[1] / "tests" / "acceptance" / "native"


def request() -> dict:
    """Create deliberately synthetic build metadata for schema-only tests."""
    snapshot = {"head": "a" * 40, "status_sha256": "b" * 64, "dirty_files": []}
    system, arch, target = acceptance.native_identity()
    interpreter = str(Path(sys.executable).resolve(strict=True))
    return {
        "schema": 1, "run_id": "synthetic-only",
        "candidate_root": str(Path.cwd().resolve()), "git": interpreter,
        "git_sha256": "1" * 64, "python_sha256": "2" * 64,
        "source": snapshot, "version": "0.1.0", "cargo_metadata": interpreter,
        "cargo_metadata_sha256": "c" * 64,
        "per_command_seconds": 1, "whole_run_seconds": 5,
        "binaries": [{
            "role": "default", "path": interpreter, "sha256": "d" * 64,
            "bytes": 100, "package": "fabric-cli", "target_name": "fab-rs",
            "target": target, "features": [], "default_features": False,
            "resolved_features": [],
            "source_snapshot_sha256": acceptance.digest(acceptance.canonical(snapshot)),
            "rustc": "synthetic-not-executed", "cargo": "synthetic-not-executed",
            "build_command": ["synthetic-not-executed"],
            "custody_ref": "synthetic-only", "auxiliary_binaries": [],
        }],
    }


def helper_request() -> dict:
    """Describe synthetic helper-artifact pins without executing any updater."""
    candidate = request()
    candidate.update(per_command_seconds=60, whole_run_seconds=180)
    binary = candidate["binaries"][0]
    binary.update(
        role="bounded-helper-tests", package="fabric-cli-update", target_name="native_fixture",
        features=["fixture-harness"], resolved_features=["bounded-helper", "fixture-harness"],
        auxiliary_binaries=[
            {
                "target_name": name,
                "path": str(Path(sys.executable).parent / (name + ".synthetic")),
                "sha256": "e" * 64, "bytes": 100,
            }
            for name in sorted(acceptance.HELPER_FIXTURES)
        ],
    )
    return candidate


class ContractTests(unittest.TestCase):
    """Negative schemas, feature scope, output and immutable bindings."""

    def test_unknown_request_schema_and_fields_are_rejected(self) -> None:
        for field, value in [("schema", 2), ("unexpected", True)]:
            candidate = request()
            candidate[field] = value
            with self.assertRaises(acceptance.Rejected):
                acceptance.validate_request(candidate, acceptance.native_identity()[2])

    def test_wrong_platform_missing_feature_and_wrong_binary(self) -> None:
        for field, value, code in [
            ("target", "not-this-native-host", "wrong_platform"),
            ("features", ["repl"], "missing_or_wrong_feature"),
            ("package", "not-fabric-cli", "wrong_binary"),
        ]:
            candidate = request()
            candidate["binaries"][0][field] = value
            with self.assertRaisesRegex(acceptance.Rejected, f"^{code}$"):
                acceptance.validate_request(candidate, acceptance.native_identity()[2])

    def test_candidate_hash_drift_is_not_relabelled(self) -> None:
        candidate = request()
        candidate["source"]["dirty_files"].append({"path": "changed", "status": " M", "sha256": "0" * 64})
        with self.assertRaisesRegex(acceptance.Rejected, "^candidate_hash_drift$"):
            acceptance.validate_request(candidate, acceptance.native_identity()[2])

    def test_optional_combined_must_be_exact_not_a_feature_subset(self) -> None:
        candidate = request()
        candidate["binaries"][0].update(role="optional-combined", features=["repl"])
        with self.assertRaises(acceptance.Rejected):
            acceptance.validate_request(candidate, acceptance.native_identity()[2])
        candidate["binaries"][0]["features"] = acceptance.COMBINED[:]
        acceptance.validate_request(candidate, acceptance.native_identity()[2])

    def test_legacy_self_replace_fixture_cannot_stand_in_for_bounded_helper(self) -> None:
        candidate = request()
        candidate["binaries"][0].update(
            role="updater-native-tests", package="fabric-cli-update",
            target_name="native_fixture", features=["fixture-harness"],
        )
        with self.assertRaisesRegex(acceptance.Rejected, "^unknown_or_duplicate_binary_role$"):
            acceptance.validate_request(candidate, acceptance.native_identity()[2])

    def test_bounded_helper_requires_both_exact_dedicated_artifact_pins(self) -> None:
        candidate = helper_request()
        acceptance.validate_request(candidate, acceptance.native_identity()[2])
        for auxiliaries in [
            candidate["binaries"][0]["auxiliary_binaries"][:1],
            [
                candidate["binaries"][0]["auxiliary_binaries"][0],
                candidate["binaries"][0]["auxiliary_binaries"][0],
            ],
        ]:
            broken = copy.deepcopy(candidate)
            broken["binaries"][0]["auxiliary_binaries"] = auxiliaries
            with self.assertRaisesRegex(acceptance.Rejected, "^both_helper_fixture_artifacts_required$"):
                acceptance.validate_request(broken, acceptance.native_identity()[2])

    def test_bounded_helper_real_timeout_case_requires_explicit_sufficient_budget(self) -> None:
        for field, value in [("per_command_seconds", 10), ("whole_run_seconds", 60)]:
            candidate = helper_request()
            candidate[field] = value
            with self.assertRaisesRegex(acceptance.Rejected, "^helper_lifecycle_budget_required$"):
                acceptance.validate_request(candidate, acceptance.native_identity()[2])

    def test_bounded_helper_selectors_are_exact_not_legacy_or_protocol_synthesis(self) -> None:
        names = acceptance.NATIVE_TESTS["bounded-helper-tests"]
        self.assertEqual(len(names), 9)
        self.assertIn("two_successive_upgrades_advance_receipt_and_reconcile_owned_state", names)
        self.assertIn("parent_lifetime_timeout_never_applies_and_can_reconcile_its_exact_stage", names)
        self.assertNotIn("owned_tiny_executable_replaces_itself_and_native_helpers_drain_after_exit", names)

    def test_unknown_output_schema_and_success_shaped_error_fail(self) -> None:
        for kind, out, err in [
            ("future-schema", b"", b""),
            ("invalid", b"", b'{"ok":true}\n'),
            ("invalid", b"", b'{"error":{"code":"invalid_arguments","message":"changed","extra":0}}\n'),
            ("invalid", b"", b'{"error":{},"error":{}}\n'),
            ("version", b"fab-rs 0.1.0\n", b"warning"),
        ]:
            with self.assertRaises(acceptance.Rejected):
                acceptance.validate_output(kind, out, err, "0.1.0")
        acceptance.validate_output("invalid", b"", json.dumps(acceptance.INVALID).encode() + b"\n", "0.1.0")

    def test_bounded_duplicate_and_nonfinite_json(self) -> None:
        for label, raw in [
            ("oversize", b" " * (acceptance.LIMIT + 1)),
            ("duplicate-key", b'{"x":0,"x":1}'),
            ("nan", b'{"x":NaN}'),
            ("positive-infinity", b'{"x":Infinity}'),
            ("negative-infinity", b'{"x":-Infinity}'),
            ("positive-exponent-overflow", b'{"x":1e9999}'),
            ("negative-exponent-overflow", b'{"x":-1e9999}'),
            ("depth-2000", b"[" * 2000 + b"0" + b"]" * 2000),
        ]:
            with self.subTest(payload=label, bytes=len(raw)):
                with self.assertRaises(acceptance.Rejected):
                    acceptance.parse(raw)
        for opening, closing in [(b"[", b"]"), (b'{"x":', b"}")]:
            with self.subTest(container=opening.decode()):
                self.assertIsNotNone(acceptance.parse(
                    opening * acceptance.JSON_DEPTH_LIMIT + b"0" +
                    closing * acceptance.JSON_DEPTH_LIMIT,
                ))
                with self.assertRaisesRegex(acceptance.Rejected, "^json_depth_limit$"):
                    acceptance.parse(
                        opening * (acceptance.JSON_DEPTH_LIMIT + 1) + b"0" +
                        closing * (acceptance.JSON_DEPTH_LIMIT + 1),
                    )
        brackets = "[" * 2000
        self.assertEqual(acceptance.parse(json.dumps({"x": brackets}).encode()), {"x": brackets})
        self.assertEqual(acceptance.parse(b'{"x":1e2}'), {"x": 100.0})

    def test_explicit_git_and_python_digest_pins_are_required(self) -> None:
        for field in ("git_sha256", "python_sha256"):
            candidate = request()
            candidate.pop(field)
            with self.assertRaisesRegex(acceptance.Rejected, "^unknown_or_missing_fields$"):
                acceptance.validate_request(candidate, acceptance.native_identity()[2])
            candidate[field] = "not-a-digest"
            with self.assertRaisesRegex(acceptance.Rejected, "^invalid_tool_digest$"):
                acceptance.validate_request(candidate, acceptance.native_identity()[2])

    def test_libtest_zero_ignored_missing_or_unknown_output_never_passes(self) -> None:
        receipt = {"exit": 0, "timeout": False, "output_limit": False,
                   "cleanup": "owned_tree_terminated"}
        valid = (
            b"running 1 test\ntest selected ... ok\n\n"
            b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "
            b"4 filtered out; finished in 0.00s\n"
        )
        self.assertTrue(acceptance.exact_test_passed(receipt, valid, b"", "selected"))
        for out in [
            b"test result: ok. 0 passed; 0 failed; 0 ignored; 1 filtered out\n",
            b'{"ok":true}',
            valid.replace(b"0 ignored", b"1 ignored"),
            valid.replace(b"selected", b"someone_else"),
            valid + b"test someone_else ... ok\n",
            valid + valid,
            valid.replace(b"0 measured", b"1 measured"),
            valid.replace(b"finished in 0.00s", b"unknown-format"),
        ]:
            self.assertFalse(acceptance.exact_test_passed(receipt, out, b"", "selected"))

    def test_environment_is_allowlisted_not_a_credential_filter(self) -> None:
        with patch.dict(os.environ, {"AZURE_CLIENT_SECRET": "not-real", "GITHUB_TOKEN": "not-real"}):
            env = acceptance.environment(Path.cwd())
        self.assertNotIn("AZURE_CLIENT_SECRET", env)
        self.assertNotIn("GITHUB_TOKEN", env)
        self.assertEqual(env["PATH"], "")
        self.assertNotIn("FAB_RUST_TENANT", env)

    def test_execution_failure_cannot_be_a_successful_libtest(self) -> None:
        receipt = {"exit": 0, "timeout": False, "output_limit": False,
                   "cleanup": "owned_tree_terminated", "execution_error": "process_execution_failure"}
        out = (
            b"test selected ... ok\n\n"
            b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "
            b"4 filtered out; finished in 0.00s\n"
        )
        self.assertFalse(acceptance.exact_test_passed(receipt, out, b"", "selected"))

    def test_malformed_nested_cargo_metadata_is_rejected_without_traversal(self) -> None:
        package = {"id": "p", "name": "p", "features": {}, "targets": []}
        acceptance.validate_metadata({"version": 1, "packages": [package], "workspace_members": ["p"]})
        for packages in [
            [None], [1], [{}],
            [{**package, "features": []}],
            [{**package, "features": {"wrong": [1]}}],
            [{**package, "targets": [None]}],
            [{**package, "targets": [{"name": "p", "kind": "bin"}]}],
        ]:
            with self.assertRaisesRegex(acceptance.Rejected, "^unknown_cargo_metadata$"):
                acceptance.validate_metadata({"version": 1, "packages": packages, "workspace_members": ["p"]})


class FilesystemTests(unittest.TestCase):
    """Use only owned current-directory scratch; never system temp roots."""

    def setUp(self) -> None:
        self.root = Path.cwd() / (".native-acceptance-test-" + uuid.uuid4().hex)
        self.root.mkdir()

    def tearDown(self) -> None:
        shutil.rmtree(self.root)

    def child(self, mode: str, *, seconds: float = 1, budget: float = 2,
              stdin: bytes = b"") -> tuple[dict, bytes, bytes]:
        """Run only the explicitly named synthetic standard-library child."""
        return acceptance.execute(
            [sys.executable, "-I", "-B", str(FIXTURES / "synthetic_child.py"), mode],
            self.root, acceptance.environment(self.root), seconds,
            time.monotonic() + budget, stdin,
        )

    def test_per_command_and_whole_run_deadlines_cleanup(self) -> None:
        for seconds, budget in [(0.12, 1), (1, 0.12)]:
            result, _, _ = self.child("wait", seconds=seconds, budget=budget)
            self.assertTrue(result["timeout"])
            self.assertEqual(result["cleanup"], "owned_tree_terminated")
            self.assertLess(result["elapsed_seconds"], 1.8)
            self.assertNotEqual(result["exit"], 0)

    def test_full_pipe_stdin_does_not_defeat_deadline(self) -> None:
        result, _, _ = self.child("wait", seconds=0.12, stdin=b"x" * acceptance.LIMIT)
        self.assertTrue(result["timeout"])
        self.assertEqual(result["cleanup"], "owned_tree_terminated")

    def test_slow_process_creation_retains_cleanup_ownership(self) -> None:
        original = acceptance.subprocess.Popen

        def delayed_spawn(*args: object, **kwargs: object) -> subprocess.Popen[bytes]:
            child = original(*args, **kwargs)
            time.sleep(0.15)
            return child

        with patch.object(acceptance.subprocess, "Popen", side_effect=delayed_spawn):
            result, _, _ = self.child("wait", seconds=0.05)
        self.assertTrue(result["timeout"])
        self.assertEqual(result["cleanup"], "owned_tree_terminated")
        self.assertEqual(result["cleanup_failure_codes"], [])
        self.assertLess(result["elapsed_seconds"], 1.8)

    @unittest.skipIf(os.name == "nt", "requires_native_POSIX_PTY")
    def test_raw_pty_cursor_flood_without_input_reads_is_bounded_and_reaped(self) -> None:
        for seconds, budget in [(0.2, 1), (1, 0.2)]:
            result, out, _ = acceptance.execute(
                [sys.executable, "-I", "-B", str(FIXTURES / "synthetic_child.py"), "cursor-flood"],
                self.root, acceptance.environment(self.root), seconds,
                time.monotonic() + budget,
                terminal={"name": "quit", "input": "quit\r"},
            )
            self.assertTrue(
                result["timeout"] or result["output_limit"] or result["terminal"]["input_queue_limit"]
            )
            self.assertTrue(result["terminal"]["raw_observed"])
            self.assertGreater(result["terminal"]["cursor_queries"], 0)
            self.assertLessEqual(result["terminal"]["input_bytes_written"], len(out) * 2 + 5)
            self.assertLessEqual(len(out), acceptance.LIMIT)
            self.assertLess(result["elapsed_seconds"], 1.8)
            self.assertEqual(result["cleanup"], "owned_tree_terminated")
            with self.assertRaises(ProcessLookupError):
                os.kill(result["pid"], 0)

    def test_output_is_bounded_and_receipt_contains_only_hashes(self) -> None:
        result, out, _ = self.child("flood")
        self.assertTrue(result["output_limit"])
        self.assertEqual(len(out), acceptance.LIMIT)
        result, out, _ = self.child("secret")
        self.assertIn(b"synthetic-secret", out)
        self.assertNotIn("synthetic-secret", json.dumps(result))
        self.assertEqual(result["stdout_sha256"], acceptance.digest(out))

    def test_wrong_binary_bytes_and_pinned_record_drift(self) -> None:
        binary = request()["binaries"][0]
        binary.update(path=str(self.root / "not-binary"), bytes=3, sha256=acceptance.digest(b"abc"))
        Path(binary["path"]).write_bytes(b"abc")
        with self.assertRaises(acceptance.Rejected):
            acceptance.verify_binary(binary, time.monotonic() + 1)
        record = self.root / "record.json"
        record.write_text('{"schema":1}', encoding="utf-8")
        with self.assertRaisesRegex(acceptance.Rejected, "^record_hash_drift$"):
            acceptance.pinned_json(record, "0" * 64, time.monotonic() + 1)

    def test_empty_file_hash_and_json_reads_obey_whole_run_deadline(self) -> None:
        path = self.root / "empty"
        path.write_bytes(b"")
        for operation in (acceptance.file_hash, acceptance.json_bytes):
            with self.assertRaisesRegex(acceptance.Rejected, "^whole_run_deadline$"):
                operation(path, time.monotonic() - 1)

    def test_pinned_json_hashes_the_same_bounded_bytes_it_parses(self) -> None:
        path = self.root / "record.json"
        raw = b'{"schema":1}'
        path.write_bytes(raw)
        with patch.object(acceptance, "json_bytes", return_value=raw) as read:
            self.assertEqual(
                acceptance.pinned_json(path, acceptance.digest(raw), time.monotonic() + 1),
                {"schema": 1},
            )
        read.assert_called_once()
        path.write_bytes(b" " * (acceptance.LIMIT + 1))
        with self.assertRaisesRegex(acceptance.Rejected, "^json_limit$"):
            acceptance.json_bytes(path, time.monotonic() + 1)

    def test_tool_digest_drift_fails_before_git_execution(self) -> None:
        git = self.root / "synthetic-git"
        git.write_bytes(b"not-executable-not-invoked")
        with self.assertRaisesRegex(acceptance.Rejected, "^tool_hash_drift$"):
            acceptance.verify_tools(git, "0" * 64, "0" * 64, time.monotonic() + 1)

    def test_failed_spawn_retains_static_error_and_truthful_cleanup(self) -> None:
        with patch.object(acceptance.subprocess, "Popen", side_effect=OSError("must-not-leak")):
            result, out, err = self.child("secret")
        self.assertIsNone(result["pid"])
        self.assertIsNone(result["exit"])
        self.assertEqual(result["cleanup"], "no_child_created")
        self.assertEqual(result["execution_error"], "process_execution_failure")
        self.assertNotIn("must-not-leak", json.dumps(result))
        self.assertEqual((out, err), (b"", b""))

    def test_unconfirmed_cleanup_is_explicit_and_never_relabelled(self) -> None:
        if os.name == "nt":
            close = acceptance.WindowsTree.close

            def unconfirmed(tree: acceptance.WindowsTree) -> bool:
                close(tree)
                return False

            with patch.object(acceptance.WindowsTree, "close", unconfirmed):
                result, _, _ = self.child("secret")
            failure = "job_members_unconfirmed"
        else:
            killpg = os.killpg

            def probe_failure(pid: int, sig: int) -> None:
                if sig == 0:
                    raise PermissionError("must-not-leak")
                killpg(pid, sig)

            with patch.object(acceptance.os, "killpg", side_effect=probe_failure):
                result, _, _ = self.child("secret")
            failure = "process_group_probe_failed"
        self.assertEqual(result["cleanup"], "unconfirmed")
        self.assertIn(failure, result["cleanup_failure_codes"])
        self.assertNotIn("must-not-leak", json.dumps(result))

    def test_auxiliary_alias_cannot_count_as_a_second_pinned_binary(self) -> None:
        binary = request()["binaries"][0]
        header = bytearray(128)
        arm = binary["target"].startswith("aarch64")
        if "windows" in binary["target"]:
            header[:2] = b"MZ"
            struct.pack_into("<I", header, 60, 64)
            header[64:68] = b"PE\0\0"
            struct.pack_into("<H", header, 68, 0xAA64 if arm else 0x8664)
        elif "linux" in binary["target"]:
            header[:6] = b"\x7fELF\x02\x01"
            struct.pack_into("<H", header, 18, 183 if arm else 62)
        else:
            header[:4] = b"\xcf\xfa\xed\xfe"
            struct.pack_into("<I", header, 4, 0x100000C if arm else 0x1000007)
        path = self.root / "synthetic-header-never-executed"
        path.write_bytes(header)
        binary.update(path=str(path), bytes=len(header), sha256=acceptance.digest(header))
        alias = self.root / "alias"
        alias.mkdir()
        binary["auxiliary_binaries"] = [{
            "path": str(alias / ".." / path.name), "bytes": len(header),
            "sha256": acceptance.digest(header),
        }]
        with self.assertRaisesRegex(acceptance.Rejected, "^duplicate_binary_path$"):
            acceptance.verify_binary(binary, time.monotonic() + 1)

    def test_partial_coverage_and_postrun_drift_cannot_succeed(self) -> None:
        for drift in (False, True):
            with self.subTest(drift=drift):
                root = self.root / ("drift" if drift else "partial")
                root.mkdir()
                source = self.root / ("source-drift" if drift else "source-partial")
                source.mkdir()
                candidate = request()
                candidate.update(candidate_root=str(source), binaries=[])
                system, arch, _ = acceptance.native_identity()
                host = {
                    "schema": 1, "registered_host_ref": "synthetic-not-host-evidence",
                    "system": system, "architecture": arch, "platform": platform.platform(),
                    "native": True, "emulated": False, "provider_credentials_absent": True,
                    "outbound_blocked": True, "isolation_evidence_ref": "synthetic-only",
                }
                lease = {
                    "schema": 1, "controller_ref": "synthetic-only",
                    "run_id": candidate["run_id"], "artifact_root": str(root),
                    "expires_utc": (datetime.now(timezone.utc) + timedelta(minutes=1)).isoformat(),
                    "policy": acceptance.MUTEX, "trusted_binary_execution": True,
                }
                changed = copy.deepcopy(candidate["source"])
                changed["head"] = "e" * 40
                with patch.object(acceptance, "verify_tools", return_value={
                    "git_sha256": candidate["git_sha256"],
                    "interpreter_sha256": candidate["python_sha256"],
                }), patch.object(acceptance, "source_snapshot", side_effect=[
                    candidate["source"], changed if drift else candidate["source"],
                ]), patch.object(acceptance, "pinned_json", return_value={
                    "version": 1, "packages": [], "workspace_members": [],
                }):
                    report, code = acceptance.run(candidate, host, lease, root, time.monotonic())
                self.assertNotEqual(code, 0)
                self.assertEqual(report["offline_result"], "failed" if drift else "partial")
                self.assertFalse(any(c["status"] == "passed" for c in report["cases"]))
                self.assertTrue((root / candidate["run_id"] / "report.json").is_file())
                helper = next(case for case in report["cases"] if case["id"] == "update/bounded-helper-native")
                self.assertEqual(helper["status"], "blocked")
                if drift:
                    self.assertEqual(report["fatal"], "candidate_hash_drift")

    def test_wrong_registered_host_refuses_before_creating_run(self) -> None:
        with self.assertRaises(acceptance.Rejected):
            acceptance.run(request(), {"schema": 99}, {}, self.root, time.monotonic())
        self.assertEqual(list(self.root.iterdir()), [])

    def fake_git(self, repository: Path, status: bytes) -> tuple[object, list]:
        """Return precise Git argv responses without executing any repository code."""
        commands = []

        def execute(argv: list, cwd: Path, env: dict, seconds: float,
                    deadline: float) -> tuple[dict, bytes, bytes]:
            self.assertEqual(argv[:6], [
                sys.executable, "--no-pager", "-c", "core.fsmonitor=false",
                "-c", "status.renames=false",
            ])
            commands.append((argv[6:], cwd))
            if argv[6:] == ["rev-parse", "--show-toplevel"]:
                out = (str(repository) + "\n").encode()
            elif argv[6:] == ["rev-parse", "--verify", "HEAD"]:
                out = b"a" * 40 + b"\n"
            else:
                self.assertEqual(argv[6:], [
                    "status", "--porcelain=v1", "-z", "--untracked-files=all", "--ignore-submodules=none",
                ])
                out = status
            return {
                "exit": 0, "timeout": False, "output_limit": False,
                "cleanup": "owned_tree_terminated",
            }, out, b""

        return execute, commands

    def test_source_snapshot_rejects_candidate_subdirectory_before_dirty_hashing(self) -> None:
        repository = self.root / "repository"
        nested = repository / "rust"
        nested.mkdir(parents=True)
        (nested / "tracked.rs").write_text("before", encoding="utf-8")
        fake, commands = self.fake_git(repository, b" M rust/tracked.rs\0")
        with patch.object(acceptance, "execute", side_effect=fake):
            with self.assertRaisesRegex(acceptance.Rejected, "^candidate_root_not_repository_top_level$"):
                acceptance.source_snapshot(nested, Path(sys.executable), time.monotonic() + 1, self.root)
        self.assertEqual(commands, [(["rev-parse", "--show-toplevel"], nested)])

    def test_source_snapshot_hashes_root_relative_dirty_bytes_and_detects_changes(self) -> None:
        repository = self.root / "repository"
        nested = repository / "rust"
        nested.mkdir(parents=True)
        source = nested / "tracked.rs"
        source.write_bytes(b"before")
        fake, commands = self.fake_git(repository, b" M rust/tracked.rs\0")
        with patch.object(acceptance, "execute", side_effect=fake):
            before = acceptance.source_snapshot(
                repository, Path(sys.executable), time.monotonic() + 1, self.root,
            )
            source.write_bytes(b"after")
            after = acceptance.source_snapshot(
                repository, Path(sys.executable), time.monotonic() + 1, self.root,
            )
        self.assertEqual(before["dirty_files"][0]["sha256"], acceptance.digest(b"before"))
        self.assertEqual(after["dirty_files"][0]["sha256"], acceptance.digest(b"after"))
        self.assertNotEqual(before, after)
        self.assertTrue(all(cwd == repository for _, cwd in commands))

    def test_source_snapshot_rejects_missing_nondeleted_dirty_file(self) -> None:
        repository = self.root / "repository"
        repository.mkdir()
        for state in (b" M", b"??", b"XD"):
            fake, _ = self.fake_git(repository, state + b" rust/missing.rs\0")
            with patch.object(acceptance, "execute", side_effect=fake):
                with self.assertRaisesRegex(acceptance.Rejected, "^missing_non_deleted_source_file$"):
                    acceptance.source_snapshot(
                        repository, Path(sys.executable), time.monotonic() + 1, self.root,
                    )

    def test_source_snapshot_records_null_only_for_porcelain_deletion(self) -> None:
        repository = self.root / "repository"
        repository.mkdir()
        for state in (" D", "D ", "AD", "MD", "DD", "DU", "UD"):
            fake, _ = self.fake_git(repository, state.encode() + b" rust/deleted.rs\0")
            with patch.object(acceptance, "execute", side_effect=fake):
                snapshot = acceptance.source_snapshot(
                    repository, Path(sys.executable), time.monotonic() + 1, self.root,
                )
            self.assertEqual(snapshot["dirty_files"], [
                {"path": "rust/deleted.rs", "status": state, "sha256": None},
            ])

if __name__ == "__main__":
    unittest.main()
