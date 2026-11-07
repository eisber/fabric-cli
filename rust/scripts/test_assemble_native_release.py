# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Synthetic offline assembly contracts, never production signing evidence.

Run from the controller artifact directory, not the checkout. All bytes,
signer references and mocked cryptographic outcomes are explicitly synthetic.
No subprocess, terminal, authentication, download or network call is permitted.
"""

from __future__ import annotations

import copy
import json
import os
import stat
import struct
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

import assemble_native_release as release


FIXTURES = Path(__file__).resolve().parents[1] / "tests" / "acceptance" / "release_assembly"


def binary(target: str, marker: bytes = b"fixture-cli") -> bytes:
    """Produce only the small native headers accepted by the current protocol."""
    data = bytearray(128)
    operating_system, architecture = release.TARGETS[target]
    if operating_system == "windows":
        data[:2] = b"MZ"
        struct.pack_into("<I", data, 0x3c, 64)
        data[64:68] = b"PE\0\0"
        struct.pack_into("<H", data, 68, 0x8664 if architecture == "x86_64" else 0xaa64)
        struct.pack_into("<H", data, 86, 2)
        struct.pack_into("<H", data, 88, 0x20b)
    elif operating_system == "linux":
        data[:7] = b"\x7fELF\x02\x01\x01"
        struct.pack_into("<HH", data, 16, 2, 62 if architecture == "x86_64" else 183)
    else:
        data[:4] = b"\xcf\xfa\xed\xfe"
        struct.pack_into("<I", data, 4, 0x01000007 if architecture == "x86_64" else 0x0100000c)
        struct.pack_into("<I", data, 12, 2)
    data[100:100 + len(marker)] = marker
    return bytes(data)


class AssemblyTests(unittest.TestCase):
    """Exercise packaging and refusal behavior using independently held fake pins."""

    def setUp(self) -> None:
        repository = Path(__file__).resolve().parents[2]
        cwd = Path.cwd().resolve()
        if cwd.is_relative_to(repository):
            raise RuntimeError("Run these tests from the controller artifact directory")
        self.temporary = tempfile.TemporaryDirectory(prefix="native-assembly-unit-", dir=cwd)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.expected_path = self.root / "expected.json"
        self.build_path = self.root / "build.json"
        self.cli_path = self.root / "held-cli"
        self.helper_path = self.root / "held-helper"
        self.policy_path = self.root / "approved-policy.json"
        self.attestation_path = self.root / "approved-os-attestation.json"
        self.update_signature = self.root / "external-update.sig"
        self.assembly_signature = self.root / "external-assembly.sig"
        self.update_signature.write_bytes(b"u" * 64)
        self.assembly_signature.write_bytes(b"a" * 64)
        self.output = self.root / "assembled"
        self.workspace = self.root / "verification-work"
        self.verifier_path = self.root / "approved-verifier-never-execute"
        self.verifier_path.write_bytes(b"SYNTHETIC NEVER EXECUTE")
        self.real_execute = release.execute
        self.guard = patch.object(release, "execute",
                                  side_effect=AssertionError("Execution forbidden"))
        self.guard.start()
        self.addCleanup(self.guard.stop)
        self.prepare()

    def prepare(self, target: str = "x86_64-pc-windows-msvc",
                custody: str = "standalone", features: list[str] | None = None) -> None:
        """Create a complete synthetic build, policy and controller expectation."""
        features = ["self-update"] if features is None else features
        operating_system, architecture = release.TARGETS[target]
        self.expected = {
            "schema": 1, "product": "fab-rs", "repository": release.REPOSITORY,
            "source_commit": "a" * 40, "dirty_manifest_sha256": None,
            "target": target,
            "platform": {"os": operating_system, "architecture": architecture,
                         "minimum_os": "declared-synthetic-floor",
                         "runtime_floor": "declared-synthetic-runtime",
                         "evidence_ref": "synthetic://floor-not-a-real-acceptance"},
            "version": "1.2.3", "tag": "rust-v1.2.3", "channel": "stable",
            "variant": release.feature_identity(features)[1],
            "explicit_features": features, "public_key_hex": "1" * 64,
            "build_record_sha256": "0" * 64, "artifacts": {},
            "installation": {
                "custody": custody,
                "root": r"C:\Fabric" if operating_system == "windows" else "/opt/fabric",
                "windows_system_root": r"C:\Windows" if operating_system == "windows" else None,
            },
            "issued_at": 1000, "expires_at": 2000,
            "signing_policy_sha256": None, "os_attestation_sha256": None,
        }
        self.cli_path.write_bytes(binary(target))
        self.helper_path.write_bytes(binary(target, b"fixture-helper"))
        for role, name, path in zip(("cli", "helper"), release.names(self.expected),
                                    (self.cli_path, self.helper_path)):
            data = path.read_bytes()
            self.expected["artifacts"][role] = {
                "role": role, "filename": name,
                "sha256": release.sha(data), "bytes": len(data),
            }
        self.policy = {
            "schema": 1, "public_key_hex": self.expected["public_key_hex"],
            "public_key_policy_ref": "synthetic://approved-public-key-policy",
            "native_signer_policy_ref": "synthetic://native-signer-certificate-policy",
            "native_verification_policy_ref": "synthetic://native-verification-tools-policy",
            "windows_signing_profile_ref": "synthetic://internal-profile-NOT-REAL",
            "apple_developer_id_ref": "synthetic://developer-id-NOT-REAL",
            "apple_notarization_policy_ref": "synthetic://notarization-NOT-REAL",
            "linux_signer_ref": "synthetic://gpg-policy-NOT-REAL",
            "production_workflow_ref": "synthetic://production-workflow-NOT-REAL",
            "release_authorization_ref": "synthetic://authorization-NOT-REAL",
            "verifier": {
                "kind": "openssl-ed25519-v1", "path": str(self.verifier_path),
                "sha256": release.sha(self.verifier_path.read_bytes()),
                "bytes": self.verifier_path.stat().st_size,
                "version": "OpenSSL 3.5.0 SYNTHETIC", "approval_ref": "synthetic://approval",
                "windows_system_root": r"C:\Windows" if os.name == "nt" else None,
            },
        }
        self.seal()

    def seal(self) -> None:
        """Pin fixture records exactly as an external controller would."""
        e = self.expected
        self.build = {
            "schema": 1, "source_commit": e["source_commit"],
            "dirty_manifest_sha256": e["dirty_manifest_sha256"],
            "toolchain": "1.99.1", "rustc_sha256": "b" * 64,
            "cargo_sha256": "c" * 64, "lockfile_sha256": "d" * 64,
            "build_command_ref": "synthetic://exact-separate-cli-and-helper-commands",
        }
        for role, package, name in (("cli", "fabric-cli", "fab-rs"),
                                    ("helper", "fabric-cli-update", "fab-rs-update-helper")):
            features = e["explicit_features"] if role == "cli" else ["bounded-helper"]
            resolved = (release.feature_identity(features)[0] if role == "cli"
                        else ["bounded-helper"])
            self.build[role] = {
                "package": package, "bin": name, "target": e["target"],
                "host_target": e["target"], "profile": "release",
                "default_features": False, "explicit_features": features,
                "resolved_features": resolved, "artifact": e["artifacts"][role],
                "compiled_pins": {
                    "FAB_RELEASE_PUBLIC_KEY": e["public_key_hex"],
                    "FAB_RELEASE_CHANNEL": e["channel"],
                    "FAB_RELEASE_VARIANT": e["variant"],
                    "FAB_RELEASE_INSTALL_CUSTODY": (
                        "standalone" if e["installation"]["custody"] == "standalone"
                        else "package-manager"
                    ),
                },
            }
        self.build_path.write_bytes(release.encode(self.build))
        e["build_record_sha256"] = release.sha(self.build_path.read_bytes())
        self.policy["public_key_hex"] = e["public_key_hex"]
        self.policy_path.write_bytes(release.encode(self.policy))
        e["signing_policy_sha256"] = release.sha(self.policy_path.read_bytes())
        kind, signer = {
            "windows": ("authenticode", "windows_signing_profile_ref"),
            "macos": ("developer-id-notarized", "apple_developer_id_ref"),
            "linux": ("gpg-detached", "linux_signer_ref"),
        }[e["platform"]["os"]]
        self.attestation = {
            "schema": 1, "target": e["target"], "version": e["version"], "tag": e["tag"],
            "policy_sha256": e["signing_policy_sha256"],
            "verification_tool_ref": "synthetic://os-signature-validator",
            "evidence_ref": "synthetic://external-approved-attestation",
            "files": [
                {"role": role, "sha256": e["artifacts"][role]["sha256"],
                 "bytes": e["artifacts"][role]["bytes"], "signature_kind": kind,
                 "signer_ref": self.policy[signer], "timestamp_ref": "synthetic://timestamp",
                 "notarization_ref": ("synthetic://notarization"
                                      if e["platform"]["os"] == "macos" else None),
                 "verified": True}
                for role in ("cli", "helper")
            ],
        }
        self.attestation_path.write_bytes(release.encode(self.attestation))
        e["os_attestation_sha256"] = release.sha(self.attestation_path.read_bytes())
        self.write_expected()

    def write_expected(self) -> None:
        self.expected_path.write_bytes(release.encode(self.expected))
        self.expect_sha = release.sha(self.expected_path.read_bytes())

    def test_v2_feature_identity_has_fixed_cross_language_vectors(self) -> None:
        """New auth features stay distinct without changing legacy identities."""
        cases = [
            (["interactive", "self-update"],
             ["interactive", "native-host", "self-update"],
             "f2-62b5bbb2c7c88db4f19bf88fdeeaff3c1372549262a637cad370d191d14c"),
            (["certificate-store-pkcs11", "self-update"],
             ["certificate-store-pkcs11", "headless", "native-host", "self-update"],
             "f2-2b218b712ab89d8dd0792c0537d773a0bb2445cf1888ec116dee2f782f0a"),
            (["certificate-store-macos", "self-update"],
             ["certificate-store-macos", "headless", "native-host", "self-update"],
             "f2-0b36a624b0d55e28806b447a9cd158e155f6bccee71d6aec00cb7dee18cb"),
        ]
        for explicit, resolved, identity in cases:
            with self.subTest(features=explicit):
                self.assertEqual(release.feature_identity(explicit), (resolved, identity))
                self.assertLessEqual(len(identity), 64)
        self.assertEqual(release.feature_identity(["self-update"]),
                         (["self-update"], "self-update"))

    def assemble(self, dirty: Path | None = None) -> dict:
        return release.assemble(self.expected_path, self.expect_sha, self.build_path,
                                self.cli_path, self.helper_path, self.output, 1500, dirty)

    def verify(self, bundle: Path | None = None) -> dict:
        return release.verify_candidate(bundle or self.output / "bundle",
                                        self.expected_path, self.expect_sha, 1500)

    def promote(self, allow: bool = True) -> dict:
        return release.signed_operation(
            self.output / "bundle", self.expected_path, self.expect_sha, 1500,
            self.policy_path, self.workspace, allow, self.root / "promoted",
            self.update_signature, self.assembly_signature, self.attestation_path,
        )

    def mock_verifier(self, command: list[str], cwd: Path,
                      system_root: str | None, log_name: str) -> bytes:
        """Inspect exact OpenSSL inputs; never claim genuine cryptographic proof."""
        self.assertEqual(command[0], str(self.verifier_path))
        if command[1:] == ["version"]:
            return self.policy["verifier"]["version"].encode() + b"\n"
        self.assertEqual(command[1:4], ["pkeyutl", "-verify", "-pubin"])
        self.assertEqual(command[6:9], ["-keyform", "DER", "-rawin"])
        message = Path(command[command.index("-in") + 1])
        signature = Path(command[command.index("-sigfile") + 1])
        self.assertTrue(message.is_relative_to(cwd))
        self.assertEqual(len(signature.read_bytes()), 64)
        key = Path(command[command.index("-inkey") + 1]).read_bytes()
        self.assertEqual(key.hex(), "302a300506032b6570032100" + "1" * 64)
        original = (release.ASSEMBLY_NAME if log_name == "verify-0"
                    else release.names(self.expected)[2])
        self.assertEqual(message.read_bytes(), (self.output / "bundle" / original).read_bytes())
        return b"synthetic success only"

    def test_six_target_two_binary_inventory_and_protocol_fields(self) -> None:
        fields = set(json.loads((FIXTURES / "protocol-v1-fields.json").read_bytes()))
        for index, target in enumerate(release.TARGETS):
            with self.subTest(target=target):
                self.prepare(target)
                self.output = self.root / f"assembled-{index}"
                result = self.assemble()
                self.assertEqual(result["status"], "unsigned-engineering-candidate")
                self.assertFalse(result["publishable"])
                self.assertEqual(self.verify()["assembly_sha256"], result["assembly_sha256"])
                bundle = self.output / "bundle"
                update = json.loads((bundle / release.names(self.expected)[2]).read_bytes())
                self.assertEqual(set(update), fields)
                self.assertNotIn("source_commit", update)
                receipt = json.loads((bundle / ".fab-rs-install.json").read_bytes())
                self.assertEqual(receipt["schema"], 2)
                self.assertEqual(receipt["helper_sha256"],
                                 self.expected["artifacts"]["helper"]["sha256"])
                self.assertEqual(receipt["windows_system_root"],
                                 self.expected["installation"]["windows_system_root"])

    def test_package_managers_never_receive_standalone_receipt(self) -> None:
        cases = (("msi", "x86_64-pc-windows-msvc"), ("winget", "x86_64-pc-windows-msvc"),
                 ("homebrew", "aarch64-apple-darwin"), ("deb", "x86_64-unknown-linux-gnu"),
                 ("rpm", "aarch64-unknown-linux-gnu"))
        for index, (manager, target) in enumerate(cases):
            with self.subTest(manager=manager):
                self.prepare(target, manager)
                self.output = self.root / f"manager-{index}"
                self.assemble()
                self.verify()
                bundle = self.output / "bundle"
                self.assertFalse((bundle / ".fab-rs-install.json").exists())
                record = json.loads((bundle / "manager-custody-v1.json").read_bytes())
                self.assertFalse(record["auto_update"])
                self.assertEqual(record["custody"], manager)

    def test_current_feature_closure_and_variant_order(self) -> None:
        features = ["headless", "native-read", "repl", "self-update"]
        self.prepare(features=features)
        self.assertEqual(self.expected["variant"],
                         "self-update+headless+native-host+native-read+repl+windows-broker")
        self.assemble()
        self.verify()

    def test_default_candidate_not_updatable_production(self) -> None:
        self.prepare(features=[])
        result = self.assemble()
        self.assertIn("self-update-feature-required-for-production-product", result["blockers"])
        self.assertEqual(self.promote()["status"], "blocked")

    def test_all_feature_variant_retains_certificate_without_exceeding_protocol_bound(self) -> None:
        features = ["certificate-store-windows", "headless", "native-host",
                    "native-read", "repl", "self-update", "windows-broker"]
        self.prepare(features=features)
        self.assertEqual(
            self.expected["variant"],
            "self-update+certificate-store-windows+native-read+repl",
        )
        self.assertLessEqual(len(self.expected["variant"]), 64)
        self.assemble()
        self.verify()

    def test_missing_public_key_policy_and_attestation_reported_together(self) -> None:
        self.expected["public_key_hex"] = None
        self.seal()
        self.expected["os_attestation_sha256"] = None
        self.policy["windows_signing_profile_ref"] = None
        self.policy["production_workflow_ref"] = None
        self.policy_path.write_bytes(release.encode(self.policy))
        self.expected["signing_policy_sha256"] = release.sha(self.policy_path.read_bytes())
        self.write_expected()
        self.assemble()
        result = self.promote()
        self.assertEqual(result["status"], "blocked")
        self.assertGreaterEqual(len(result["blockers"]), 4)
        self.assertFalse((self.root / "promoted").exists())
        self.assertFalse(self.workspace.exists())

    def test_macos_notarization_policy_is_not_optional(self) -> None:
        self.prepare("aarch64-apple-darwin")
        self.policy["apple_notarization_policy_ref"] = None
        self.seal()
        self.assemble()
        self.assertIn("missing-apple-notarization-policy-ref", self.promote()["blockers"])

    def test_dirty_manifest_bound_and_cannot_promote(self) -> None:
        dirty = self.root / "dirty.json"
        dirty.write_bytes(release.encode({
            "schema": 1, "base_commit": "a" * 40,
            "entries": [{"path": "rust/example.rs", "status": "modified",
                         "sha256": "e" * 64, "bytes": 12, "mode": "100644"}],
        }))
        self.expected["dirty_manifest_sha256"] = release.sha(dirty.read_bytes())
        self.seal()
        with self.assertRaises(release.AssemblyError):
            self.assemble()
        self.assemble(dirty)
        self.verify()
        self.assertIn("clean-reviewed-source-required", self.promote()["blockers"])

    def test_duplicate_dirty_paths_and_traversal_rejected(self) -> None:
        for path in ("../outside", r"C:\outside", "/absolute", "a//b"):
            with self.subTest(path=path):
                raw = release.encode({"schema": 1, "base_commit": "a" * 40, "entries": [
                    {"path": path, "status": "deleted", "sha256": None,
                     "bytes": None, "mode": None},
                ]})
                e = copy.deepcopy(self.expected)
                e["dirty_manifest_sha256"] = release.sha(raw)
                with self.assertRaises(release.AssemblyError):
                    release.validate_dirty(raw, e)
        item = {"path": "rust/a.rs", "status": "deleted",
                "sha256": None, "bytes": None, "mode": None}
        raw = release.encode({"schema": 1, "base_commit": "a" * 40,
                              "entries": [item, item]})
        e = copy.deepcopy(self.expected)
        e["dirty_manifest_sha256"] = release.sha(raw)
        with self.assertRaises(release.AssemblyError):
            release.validate_dirty(raw, e)

    def test_wrong_expected_digest_rejected(self) -> None:
        self.expect_sha = "f" * 64
        with self.assertRaises(release.AssemblyError):
            self.assemble()

    def test_duplicate_json_unknown_schema_nonfinite_and_unknown_fields(self) -> None:
        raw = self.expected_path.read_bytes()
        variants = (
            raw.replace(b"{", b'{"schema":1,', 1),
            raw.replace(b'"schema": 1', b'"schema": 2'),
            raw.replace(b'"issued_at": 1000', b'"issued_at": NaN'),
            raw.replace(b'"issued_at": 1000', b'"issued_at": 1e999'),
            raw.replace(b"{", b'{"unreviewed":true,', 1),
        )
        for candidate in variants:
            with self.subTest(raw=candidate[:20]):
                self.expected_path.write_bytes(candidate)
                with self.assertRaises(release.AssemblyError):
                    release.load_expected(self.expected_path, release.sha(candidate))

    def test_minima_channel_product_and_version_require_explicit_pins(self) -> None:
        mutations = (
            ("repository", "eisber/fabric-cli"), ("tag", "v1.2.3"),
            ("channel", None), ("source_commit", "558d0dd"),
            ("version", "01.2.3"), ("version", "1.2.3-01"),
            ("schema", True), ("public_key_hex", "0" * 64),
        )
        original = copy.deepcopy(self.expected)
        for field, value in mutations:
            with self.subTest(field=field):
                self.expected = copy.deepcopy(original)
                self.expected[field] = value
                self.write_expected()
                with self.assertRaises(release.AssemblyError):
                    release.load_expected(self.expected_path, self.expect_sha)
        self.expected = copy.deepcopy(original)
        self.expected["platform"]["minimum_os"] = None
        self.write_expected()
        with self.assertRaises(release.AssemblyError):
            release.load_expected(self.expected_path, self.expect_sha)

    def test_no_helper_pin_no_helper_binary_and_wrong_role_rejected(self) -> None:
        self.helper_path.unlink()
        with self.assertRaises(OSError):
            self.assemble()
        self.helper_path.write_bytes(binary(self.expected["target"], b"fixture-helper"))
        self.expected["artifacts"]["helper"]["role"] = "cli"
        self.write_expected()
        with self.assertRaises(release.AssemblyError):
            self.assemble()

    def test_native_build_and_compiled_pins_cannot_be_substituted(self) -> None:
        mutations = (
            ("host_target", "aarch64-apple-darwin"),
            ("bin", "fabric-update-helper-fixture"),
            ("explicit_features", ["fixture-harness"]),
            ("default_features", True),
            ("compiled_pins", {
                "FAB_RELEASE_PUBLIC_KEY": "2" * 64, "FAB_RELEASE_CHANNEL": "stable",
                "FAB_RELEASE_VARIANT": "self-update",
                "FAB_RELEASE_INSTALL_CUSTODY": "standalone",
            }),
        )
        original = copy.deepcopy(self.build)
        for field, value in mutations:
            with self.subTest(field=field):
                self.build = copy.deepcopy(original)
                self.build["helper"][field] = value
                self.build_path.write_bytes(release.encode(self.build))
                self.expected["build_record_sha256"] = release.sha(self.build_path.read_bytes())
                self.write_expected()
                with self.assertRaises(release.AssemblyError):
                    self.assemble()

    def test_wrong_header_even_with_matching_hash_rejected(self) -> None:
        self.cli_path.write_bytes(binary("aarch64-apple-darwin"))
        p = self.expected["artifacts"]["cli"]
        p.update(sha256=release.sha(self.cli_path.read_bytes()), bytes=self.cli_path.stat().st_size)
        self.seal()
        with self.assertRaises(release.AssemblyError):
            self.assemble()

    def test_expires_and_future_metadata_rejected(self) -> None:
        for now in (999, 2000, True):
            with self.subTest(now=now), self.assertRaises(release.AssemblyError):
                release.check_time(self.expected, now)

    def test_drift_extra_entries_directories_and_receipt_mutation_rejected(self) -> None:
        self.assemble()
        bundle = self.output / "bundle"
        helper = bundle / release.names(self.expected)[1]
        original = helper.read_bytes()
        helper.write_bytes(original + b"drift")
        with self.assertRaises(release.AssemblyError):
            self.verify()
        helper.write_bytes(original)
        extra = bundle / "extra"
        extra.mkdir()
        with self.assertRaises(release.AssemblyError):
            self.verify()
        extra.rmdir()
        extra.write_bytes(b"extra")
        with self.assertRaises(release.AssemblyError):
            self.verify()
        extra.unlink()
        record = bundle / ".fab-rs-install.json"
        record.write_bytes(record.read_bytes().replace(b'"schema": 2', b'"schema": 1'))
        with self.assertRaises(release.AssemblyError):
            self.verify()

    def test_hardlinks_rejected(self) -> None:
        link = self.root / "helper-alias"
        try:
            os.link(self.helper_path, link)
        except OSError as error:
            self.skipTest(f"Host cannot create test hard link: {error.errno}")
        with self.assertRaises(release.AssemblyError):
            self.assemble()

    def test_symlinks_rejected_when_host_permits_creation(self) -> None:
        link = self.root / "helper-link"
        try:
            link.symlink_to(self.helper_path)
        except OSError as error:
            self.skipTest(f"Host cannot create test symlink: {error.errno}")
        with self.assertRaises(release.AssemblyError):
            release.read(link)

    def test_reparse_attribute_rejected_without_real_reparse_operation(self) -> None:
        info = type("Reparse", (), {"st_file_attributes": 0x400,
                                   "st_mode": stat.S_IFREG})()
        with patch.object(Path, "lstat", return_value=info):
            with self.assertRaises(release.AssemblyError):
                release.plain(self.helper_path)

    def test_metadata_oversize_rejected_before_parse(self) -> None:
        self.expected_path.write_bytes(b" " * (release.METADATA_CAP + 1))
        with self.assertRaises(release.AssemblyError):
            release.load_expected(self.expected_path, self.expect_sha)

    def test_windows_systemroot_required_and_never_ambient(self) -> None:
        for value in (None, r"\\server\Windows", r"C:\Windows\..\Elsewhere"):
            with self.subTest(value=value):
                self.expected["installation"]["windows_system_root"] = value
                self.write_expected()
                with self.assertRaises(release.AssemblyError):
                    self.assemble()

    def test_overwrite_rejected_and_foreign_files_retained(self) -> None:
        self.output.mkdir()
        sentinel = self.output / "foreign"
        sentinel.write_bytes(b"must remain")
        with self.assertRaises(FileExistsError):
            self.assemble()
        self.assertEqual(sentinel.read_bytes(), b"must remain")

    def test_failed_staging_removes_only_owned_files(self) -> None:
        original = release.read

        def fail_stage(path: Path, cap: int = release.METADATA_CAP) -> bytes:
            if ".staging" in path.parts:
                raise release.AssemblyError("synthetic staged read failure")
            return original(path, cap)

        with patch.object(release, "read", side_effect=fail_stage):
            with self.assertRaises(release.AssemblyError):
                self.assemble()
        self.assertFalse(self.output.exists())

    def test_signatures_require_separate_explicit_verifier_consent(self) -> None:
        self.assemble()
        with self.assertRaises(release.AssemblyError):
            self.promote(False)
        self.assertFalse(self.workspace.exists())
        self.assertFalse((self.root / "promoted").exists())

    def test_synthetic_promote_and_verify_signed_fixed_command_contract(self) -> None:
        self.assemble()
        with patch.object(release, "execute", side_effect=self.mock_verifier) as execute:
            report = self.promote()
            self.assertEqual(execute.call_count, 3)
            self.assertEqual(report["status"], "verified-signed-assembly")
            self.assertFalse(report["publishable"])
            report = release.signed_operation(
                self.root / "promoted" / "bundle", self.expected_path, self.expect_sha,
                1500, self.policy_path, self.workspace, True,
            )
            self.assertEqual(report["status"], "verified-signed-assembly")
        self.assertFalse(self.workspace.exists())
        with self.assertRaises(release.AssemblyError):
            self.verify(self.root / "promoted" / "bundle")

    def test_signature_failure_does_not_promote_and_cleans_workspace(self) -> None:
        self.assemble()

        def reject(command: list[str], cwd: Path, system_root: str | None,
                   log_name: str) -> bytes:
            if log_name == "version":
                return self.policy["verifier"]["version"].encode()
            raise release.AssemblyError("Signature verification failed")

        with patch.object(release, "execute", side_effect=reject):
            with self.assertRaises(release.AssemblyError):
                self.promote()
        self.assertFalse((self.root / "promoted").exists())
        self.assertFalse(self.workspace.exists())

    def test_verifier_version_or_digest_drift_rejected(self) -> None:
        self.assemble()
        with patch.object(release, "execute", return_value=b"OpenSSL wrong"):
            with self.assertRaises(release.AssemblyError):
                self.promote()
        self.verifier_path.write_bytes(b"drift")
        with self.assertRaises(release.AssemblyError):
            self.promote()

    def test_short_signature_and_unpinned_os_attestation_rejected(self) -> None:
        self.assemble()
        self.update_signature.write_bytes(b"short")
        with patch.object(release, "execute", side_effect=self.mock_verifier):
            with self.assertRaises(release.AssemblyError):
                self.promote()
        self.update_signature.write_bytes(b"u" * 64)
        self.attestation_path.write_bytes(b"{}")
        with self.assertRaises(release.AssemblyError):
            self.promote()

    def test_validly_pinned_but_unverified_os_attestation_rejected(self) -> None:
        self.attestation["files"][1]["verified"] = False
        self.attestation_path.write_bytes(release.encode(self.attestation))
        self.expected["os_attestation_sha256"] = release.sha(self.attestation_path.read_bytes())
        self.write_expected()
        self.assemble()
        with self.assertRaises(release.AssemblyError):
            self.promote()

    def test_signature_workspace_cannot_overlap_package(self) -> None:
        self.assemble()
        self.workspace = self.output / "bundle" / "workspace"
        with self.assertRaises(release.AssemblyError):
            self.promote()

    def test_relative_artifacts_and_inventory_path_injection_rejected(self) -> None:
        with self.assertRaises(release.AssemblyError):
            release.read(Path("relative"))
        for name in ("../escape", r"..\escape", "CON", "a.exe:stream", ".hidden", "a..b"):
            with self.subTest(name=name), self.assertRaises(release.AssemblyError):
                release.leaf(name)

    def test_verifier_execution_environment_and_output_contract_without_process(self) -> None:
        work = self.root / "process-contract"
        work.mkdir()
        process = Mock()
        process.poll.return_value = 0
        process.returncode = 0
        process.wait.return_value = 0

        def fake_start(command: list[str], **arguments: object) -> Mock:
            self.assertEqual(command, [str(self.verifier_path), "version"])
            self.assertFalse(arguments["shell"])
            self.assertEqual(arguments["cwd"], work)
            environment = arguments["env"]
            self.assertEqual(environment["PATH"], "")
            self.assertEqual(environment["OPENSSL_CONF"], os.devnull)
            allowed = {"PATH", "HOME", "USERPROFILE", "TMP", "TEMP", "TMPDIR",
                       "OPENSSL_CONF", "OPENSSL_MODULES", "SystemRoot", "WINDIR"}
            self.assertLessEqual(set(environment), allowed)
            arguments["stdout"].write(b"synthetic version\n")
            arguments["stdout"].flush()
            return process

        with patch.object(release.subprocess, "Popen", side_effect=fake_start):
            self.assertEqual(self.real_execute(
                [str(self.verifier_path), "version"], work, str(self.root), "case"
            ), b"synthetic version\n")
        self.assertEqual(list(work.iterdir()), [])
        process.kill.assert_not_called()

    def test_verifier_output_bound_kills_only_owned_mock_process(self) -> None:
        work = self.root / "process-bound"
        work.mkdir()
        process = Mock()
        process.poll.return_value = None
        process.wait.return_value = 0

        def fake_start(command: list[str], **arguments: object) -> Mock:
            arguments["stdout"].write(b"x" * 65537)
            arguments["stdout"].flush()
            return process

        with patch.object(release.subprocess, "Popen", side_effect=fake_start):
            with self.assertRaises(release.AssemblyError):
                self.real_execute([str(self.verifier_path), "version"], work,
                                  str(self.root), "case")
        process.kill.assert_called_once_with()
        self.assertEqual(list(work.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
