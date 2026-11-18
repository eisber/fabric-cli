# Copyright (c) Microsoft Corporation. Licensed under the MIT License.
"""Offline stdlib-only regression checks for the delivery scripts; no Cargo or az."""

import copy
import io
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile
import textwrap
import tomllib
import unittest
from unittest.mock import patch
import warnings
import zipfile

import check_boundaries as boundaries
import native_host
import package_native as package


def executable(target: str) -> bytes:
    """Produce just enough synthetic header bytes for format validation."""
    data = bytearray(256)
    if "windows" in target:
        data[:2] = b"MZ"
        struct.pack_into("<I", data, 60, 128)
        data[128:132] = b"PE\0\0"
        struct.pack_into("<H", data, 132, 0x8664)
    elif "linux" in target:
        data[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<H", data, 18, 62)
    else:
        data[:4] = b"\xcf\xfa\xed\xfe"
        struct.pack_into("<I", data, 4, 0x0100000C if target.startswith("aarch64") else 0x01000007)
    return bytes(data)


class ArchiveTests(unittest.TestCase):
    """Check exact archive content, identity, modes and unsafe entry rejection."""

    def contents(self, target: str) -> dict:
        """Minimal synthetic archive contents."""
        return {
            package.binary_name(target): executable(target),
            "LICENSE": b"product license", "THIRD-PARTY-NOTICES.txt": b"notices",
            "README.txt": b"prerequisites",
        }

    def test_round_trip_all_native_formats(self) -> None:
        for target in native_host.TARGETS:
            with self.subTest(target=target):
                contents = self.contents(target)
                self.assertEqual(package.unpack(package.archive_bytes(contents, target), target), contents)

    def test_wrong_architecture_rejected(self) -> None:
        with self.assertRaises(ValueError):
            package.verify_binary(executable("aarch64-apple-darwin"), "x86_64-apple-darwin")
        for target in native_host.TARGETS:
            with self.assertRaises((ValueError, struct.error)):
                package.verify_binary(b"not an executable", target)

    def test_missing_and_unexpected_files_rejected(self) -> None:
        for target in native_host.TARGETS:
            contents = self.contents(target)
            contents.pop("LICENSE")
            with self.assertRaises(ValueError):
                package.unpack(package.archive_bytes(contents, target), target)
            contents = self.contents(target)
            contents["../escape"] = b"unsafe"
            with self.assertRaises(ValueError):
                package.unpack(package.archive_bytes(contents, target), target)

    def test_tar_links_and_zip_duplicates_rejected(self) -> None:
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode="w:gz") as archive:
            info = tarfile.TarInfo("fab-rs")
            info.type = tarfile.SYMTYPE
            info.linkname = "/bin/sh"
            archive.addfile(info)
        with self.assertRaises(ValueError):
            package.unpack(stream.getvalue(), "x86_64-unknown-linux-gnu")
        stream = io.BytesIO()
        with warnings.catch_warnings(), zipfile.ZipFile(stream, "w") as archive:
            warnings.simplefilter("ignore", UserWarning)
            archive.writestr("LICENSE", b"a")
            archive.writestr("LICENSE", b"b")
        with self.assertRaises(ValueError):
            package.unpack(stream.getvalue(), "x86_64-pc-windows-msvc")


class LegalTests(unittest.TestCase):
    """No placeholder notices, and nested native-source licenses are retained."""

    def test_nested_texts_included_and_missing_texts_block(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            info = {
                "id": "test", "name": "example", "version": "1.0.0",
                "manifest_path": str(root / "Cargo.toml"),
                "source": "registry+https://github.com/rust-lang/crates.io-index",
                "license": "MIT", "license_file": None,
            }
            metadata = {"packages": [info], "resolve": {"nodes": [{"id": "test", "deps": []}]}}
            lock = {"package": [{"name": "example", "version": "1.0.0", "source": info["source"], "checksum": "b" * 64}]}
            (root / "AUTHORS").write_text("Arbitrary authors are not a license fallback", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "No shipped license"):
                package.legal_and_sbom(metadata, lock, "a" * 40, "0.1.0")
            (root / "LICENSE").write_text("Complete license text", encoding="utf-8")
            native = root / "native"
            native.mkdir()
            (native / "NOTICE").write_text("Native component notice", encoding="utf-8")
            licenses = root / "LICENSES"
            licenses.mkdir()
            (licenses / "Unicode-3.0.txt").write_text("Nested REUSE license", encoding="utf-8")
            notices, sbom = package.legal_and_sbom(metadata, lock, "a" * 40, "0.1.0")
            self.assertIn(b"Complete license text", notices)
            self.assertIn(b"Native component notice", notices)
            self.assertIn(b"Nested REUSE license", notices)
            self.assertEqual(sbom["dependencies"], [{"ref": "pkg:cargo/example@1.0.0", "dependsOn": []}])
            self.assertNotIn(str(root), json.dumps(sbom))
            info["license_file"] = "../outside"
            with self.assertRaisesRegex(ValueError, "Unusable license_file"):
                package.license_files(info)

    def reviewed_fixture(self, root: Path, name: str) -> tuple[dict, dict, dict]:
        """Create a crate identity from the real exception pins, without Cargo/network."""
        entries = tomllib.loads((package.ROOT / "packaging/license-inputs.toml").read_text())["package"]
        entry = next(item for item in entries if item["name"] == name)
        crate = root / name
        crate.mkdir()
        (crate / ".cargo_vcs_info.json").write_text(json.dumps({
            "git": {"sha1": entry["revision"]}, "path_in_vcs": entry["path_in_vcs"],
        }))
        info = {
            "id": name, "name": name, "version": entry["version"],
            "manifest_path": str(crate / "Cargo.toml"),
            "source": "registry+https://github.com/rust-lang/crates.io-index",
            "license": entry["license"], "license_file": None, "repository": entry["repository"],
        }
        lock = {"package": [{
            "name": name, "version": info["version"], "source": info["source"], "checksum": entry["checksum"],
        }]}
        return info, lock, entry

    def legal(self, info: dict, lock: dict) -> tuple[bytes, dict]:
        """Generate notices/SBOM for one fixture package."""
        metadata = {"packages": [info], "resolve": {"nodes": [{"id": info["id"], "deps": []}]}}
        return package.legal_and_sbom(metadata, lock, "a" * 40, "0.1.0")

    def test_all_vendored_inputs_include_exact_texts_notices_and_provenance(self) -> None:
        entries = tomllib.loads((package.ROOT / "packaging/license-inputs.toml").read_text())["package"]
        with tempfile.TemporaryDirectory() as temporary:
            for entry in entries:
                if any(item.get("shipped") for item in entry["files"]):
                    continue
                with self.subTest(package=entry["name"]):
                    info, lock, _ = self.reviewed_fixture(Path(temporary), entry["name"])
                    notices, sbom = self.legal(info, lock)
                    self.assertIn(b"Reviewed package-specific license choice: MIT", notices)
                    component = sbom["components"][0]
                    self.assertIn({"name": "cargo:license", "value": entry["license"]}, component["properties"])
                    for item, reference in zip(entry["files"], component["externalReferences"], strict=True):
                        data = (package.ROOT / "packaging" / item["file"]).read_bytes()
                        self.assertEqual(package.digest(data), item["sha256"])
                        self.assertIn(data, notices)
                        self.assertIn(entry["revision"], reference["url"])
                        self.assertTrue(reference["url"].endswith("/" + item["source_path"]))
                        self.assertEqual(reference["hashes"], [{"alg": "SHA-256", "content": item["sha256"]}])
                    self.assertNotIn(temporary, notices.decode() + json.dumps(sbom))
                    if info["name"] == "azure_core":
                        self.assertIn(b"License notice for `http_types::{Method, StatusCode}`", notices)
                        self.assertIn(b"Copyright (c) 2017 http-rs authors", notices)

    def test_reviewed_input_rejects_identity_changes_and_unreviewed_version(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            info, lock, _ = self.reviewed_fixture(Path(temporary), "azure_core")
            for field, value in (
                ("license", "MIT OR Apache-2.0"),
                ("repository", "https://github.com/example/unreviewed"),
                ("repository", None),
            ):
                with self.subTest(field=field, value=value):
                    changed = dict(info, **{field: value})
                    with self.assertRaisesRegex(ValueError, "Reviewed license identity mismatch"):
                        self.legal(changed, lock)
            changed_lock = copy.deepcopy(lock)
            changed_lock["package"][0]["checksum"] = "0" * 64
            with self.assertRaisesRegex(ValueError, "Reviewed license identity mismatch"):
                self.legal(info, changed_lock)
            info["version"] = "1.1.1"
            lock["package"][0]["version"] = info["version"]
            with self.assertRaisesRegex(ValueError, "No shipped license"):
                self.legal(info, lock)

    def test_reviewed_input_rejects_missing_or_changed_crate_vcs_identity(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            info, lock, entry = self.reviewed_fixture(Path(temporary), "azure_core")
            vcs_path = Path(info["manifest_path"]).parent / ".cargo_vcs_info.json"
            for revision, path in (("0" * 40, entry["path_in_vcs"]), (entry["revision"], "wrong/crate")):
                with self.subTest(revision=revision, path=path):
                    vcs_path.write_text(json.dumps({"git": {"sha1": revision}, "path_in_vcs": path}))
                    with self.assertRaisesRegex(ValueError, "Reviewed license VCS mismatch"):
                        self.legal(info, lock)
            vcs_path.unlink()
            with self.assertRaisesRegex(ValueError, "Missing reviewed license VCS identity"):
                self.legal(info, lock)

    def test_reviewed_input_rejects_tampered_text_even_with_shipped_license(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            info, lock, entry = self.reviewed_fixture(root, "azure_core")
            shutil.copytree(package.ROOT / "packaging", root / "packaging")
            (root / "packaging" / entry["files"][0]["file"]).write_text("Tampered license text")
            (Path(info["manifest_path"]).parent / "LICENSE").write_text("Shipped license")
            with patch.object(package, "ROOT", root):
                with self.assertRaisesRegex(ValueError, "Reviewed license file checksum mismatch"):
                    self.legal(info, lock)

    def test_reviewed_inputs_do_not_mask_unusable_declared_paths(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            info, lock, _ = self.reviewed_fixture(Path(temporary), "azure_core")
            for declared in ("../outside", "missing-license"):
                with self.subTest(path=declared):
                    info["license_file"] = declared
                    with self.assertRaisesRegex(ValueError, "Unusable license_file"):
                        self.legal(info, lock)

    def test_explicit_shipped_authors_input_is_hash_checked_and_preserved(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            info, lock, entry = self.reviewed_fixture(root, "r-efi")
            shutil.copytree(package.ROOT / "packaging", root / "packaging")
            # Synthetic bytes get their own fixture pin, never a production fallback.
            authors = Path(info["manifest_path"]).parent / "AUTHORS"
            text = b"Synthetic fixture copyright, license grant, and attribution.\n"
            authors.write_bytes(text)
            manifest = root / "packaging/license-inputs.toml"
            manifest.write_text(manifest.read_text().replace(entry["files"][0]["sha256"], package.digest(text)))
            with self.assertRaisesRegex(ValueError, "No shipped license"):
                package.license_files(info)
            with patch.object(package, "ROOT", root):
                notices, sbom = self.legal(info, lock)
                self.assertIn(text, notices)
                self.assertTrue(sbom["components"][0]["externalReferences"][0]["url"].endswith("/AUTHORS"))
                authors.write_bytes(text + b"unreviewed change")
                with self.assertRaisesRegex(ValueError, "Reviewed license file checksum mismatch"):
                    self.legal(info, lock)

    def test_empty_shipped_license_text_blocks_packaging(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            info, lock, _ = self.reviewed_fixture(Path(temporary), "azure_core")
            (Path(info["manifest_path"]).parent / "LICENSE").write_text(" \n")
            with self.assertRaisesRegex(ValueError, "Empty shipped license text"):
                self.legal(info, lock)

    def test_missing_checksum_blocks_and_private_source_is_not_published(self) -> None:
        info = {
            "id": "path+file:///private/path#example@1.0.0", "name": "example",
            "version": "1.0.0", "source": "git+https://private.invalid/repo",
            "license": "MIT", "license_file": None,
        }
        with self.assertRaisesRegex(ValueError, "non-public dependency source"):
            package.legal_and_sbom({"packages": [info]}, {"package": []}, "a" * 40, "0.1.0")
        info["source"] = "registry+https://github.com/rust-lang/crates.io-index"
        with self.assertRaisesRegex(ValueError, "Missing registry checksum"):
            package.legal_and_sbom({"packages": [info]}, {"package": []}, "a" * 40, "0.1.0")


class GateTests(unittest.TestCase):
    """Execute the actual embedded selector/gate scripts using synthetic Git output."""

    def snippets(self) -> list[str]:
        """Extract Python run blocks without adding YAML runtime dependencies."""
        workflow = (package.REPOSITORY / ".github/workflows/rust-ci.yml").read_text()
        return [
            textwrap.dedent(block) for block in re.findall(
                r"(?m)^        run: \|\n((?:          .*\n|\n)+)", workflow,
            )
        ]

    def test_required_gate_allows_only_deliberate_skip_or_success(self) -> None:
        gate = self.snippets()[1]
        for changes, changed, native, passes in (
            ("success", "false", "skipped", True),
            ("success", "true", "success", True),
            ("failure", "false", "skipped", False),
            ("success", "true", "skipped", False),
            ("success", "true", "cancelled", False),
            ("success", "true", "failure", False),
            ("success", "", "skipped", False),
            ("skipped", "", "skipped", False),
        ):
            with self.subTest(changes=changes, changed=changed, native=native):
                with patch.dict(os.environ, {
                    "CHANGES_RESULT": changes, "CHANGED": changed, "NATIVE_RESULT": native,
                }):
                    if passes:
                        exec(compile(gate, "Rust validation gate", "exec"), {})
                    else:
                        with self.assertRaises(SystemExit):
                            exec(compile(gate, "Rust validation gate", "exec"), {})

    def test_selector_relevant_unrelated_manual_merge_group_and_missing_base(self) -> None:
        selector = self.snippets()[0]
        for event, base, paths, expected in (
            ("pull_request", "a" * 40, "README.md\0src/example.py\0", "false"),
            ("pull_request", "a" * 40, "rust/docs/authoring/compatibility.md\0", "true"),
            ("pull_request", "a" * 40, "rust/docs/Unicode-α.md\0", "true"),
            ("merge_group", "b" * 40, ".github/workflows/rust-native.yml\0", "false"),
            ("merge_group", "b" * 40, "rust/engine/core/contracts/src/lib.rs\0", "true"),
            ("pull_request", "a" * 40, "LICENSE\0AGENTS.md\0", "false"),
            ("pull_request", "a" * 40, "rust-other/example.rs\0", "false"),
            ("pull_request", "a" * 40, "tests/test_api.py\0rust/Cargo.lock\0", "true"),
            ("push", "0" * 40, "", "true"),
            ("workflow_dispatch", "", "", "true"),
        ):
            with self.subTest(event=event, paths=paths), tempfile.TemporaryDirectory() as temporary:
                output = Path(temporary) / "output"
                with patch.dict(os.environ, {"EVENT": event, "BASE": base, "GITHUB_OUTPUT": str(output)}), \
                     patch("subprocess.check_output", return_value=paths):
                    exec(compile(selector, "Rust selector", "exec"), {})
                self.assertEqual(output.read_text(), f"rust={expected}\n")
        with patch.dict(os.environ, {"EVENT": "merge_group", "BASE": ""}):
            with self.assertRaises(SystemExit):
                exec(compile(selector, "Rust selector", "exec"), {})


class BoundaryTests(unittest.TestCase):
    """All dependency kinds are guarded, not only resolved normal edges."""

    def metadata(self, root: Path) -> dict:
        """Create a synthetic complete retained workspace metadata graph."""
        members = list(boundaries.MEMBERS)
        (root / "Cargo.toml").write_text(
            "[workspace]\nmembers = " + json.dumps(members) + '\n[workspace.package]\nversion = "0.1.0"\n'
        )
        packages = [
            {"id": name, "name": name, "manifest_path": str(root / path / "Cargo.toml"),
             "source": None, "dependencies": []}
            for path, name in boundaries.MEMBERS.items()
        ]
        return {"packages": packages, "workspace_members": [p["id"] for p in packages], "resolve": {}}

    def test_reduced_graph_and_inverted_dev_edge(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            metadata = self.metadata(root)
            boundaries.validate(metadata, root)
            metadata["packages"][0]["dependencies"].append({
                "name": "fabric-cli", "kind": "dev", "path": str(root / "clients/cli"),
            })
            with self.assertRaisesRegex(ValueError, "Forbidden dependency"):
                boundaries.validate(metadata, root)

    def test_service_broker_and_external_path_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            base = self.metadata(root)
            for name in ("fabric-sdk-workspaces", "azure-identity-broker", "other-local"):
                metadata = copy.deepcopy(base)
                metadata["packages"].append({
                    "id": name, "name": name, "manifest_path": str(root / "extra/Cargo.toml"),
                    "source": None, "dependencies": [],
                })
                with self.subTest(name=name), self.assertRaises(ValueError):
                    boundaries.validate(metadata, root)

    def test_auth_cannot_depend_on_api_or_native_identity(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for path in ("clients/cli/commands/api", "engine/core/auth/provider"):
                metadata = self.metadata(root)
                auth = next(p for p in metadata["packages"] if p["name"] == "fabric-cli-auth")
                auth["dependencies"].append({
                    "name": boundaries.MEMBERS[path], "kind": "dev", "path": str(root / path),
                })
                with self.subTest(path=path), self.assertRaisesRegex(ValueError, "Forbidden dependency"):
                    boundaries.validate(metadata, root)


class VerificationTests(unittest.TestCase):
    """Verify exact set and final bytes without native execution."""

    def test_complete_set_tampering_and_smoke_routing(self) -> None:
        target = "x86_64-unknown-linux-gnu"
        version = "0.1.0"
        source = package.command("git", "-C", str(package.REPOSITORY), "rev-parse", "HEAD")
        stem = f"fab-rs-{version}-{target}"
        archive_name = stem + ".tar.gz"
        contents = ArchiveTests().contents(target)
        contents["LICENSE"] = (package.REPOSITORY / "LICENSE").read_bytes()
        provenance = {
            "version": version, "target": target, "archive": archive_name,
            "product": "fab-rs", "source_revision": source, "unsigned_unofficial_preview": True,
            "toolchain": {"target": target, "toolchain": "1.99.1"},
            "contents_sha256": {name: package.digest(data) for name, data in contents.items()},
        }
        sbom = {
            "bomFormat": "CycloneDX", "components": [{"name": "dependency"}],
            "dependencies": [{"ref": "dependency", "dependsOn": []}],
            "metadata": {"component": {"version": version}, "properties": [
                {"name": "source:revision", "value": source},
            ]},
        }
        assets = {
            archive_name: package.archive_bytes(contents, target),
            stem + ".cdx.json": package.json_bytes(sbom),
            stem + ".provenance.json": package.json_bytes(provenance),
        }
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name, data in assets.items():
                (root / name).write_bytes(data)
            (root / "SHA256SUMS").write_text(
                "".join(f"{package.digest(data)}  {name}\n" for name, data in assets.items())
            )
            package.verify(target, root, False)
            with patch.object(package, "evidence"), patch.object(package, "smoke") as smoke:
                package.verify(target, root, True)
                smoke.assert_called_once()
            (root / "unexpected.txt").write_text("unexpected")
            with self.assertRaisesRegex(ValueError, "unexpected release assets"):
                package.verify(target, root, False)
            (root / "unexpected.txt").unlink()
            (root / archive_name).write_bytes(b"tampered")
            with self.assertRaisesRegex(ValueError, "Checksum mismatch"):
                package.verify(target, root, False)

    def test_build_and_verify_synthetic_workspace_without_cargo(self) -> None:
        target = "x86_64-unknown-linux-gnu"
        with tempfile.TemporaryDirectory() as temporary:
            repository = Path(temporary)
            root = repository / "rust"
            root.mkdir()
            metadata = BoundaryTests().metadata(root)
            (root / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "1.99.1"\n')
            lock = ""
            for component in metadata["packages"]:
                component.update(version="0.1.0", license="MIT", license_file=None)
                lock += f'[[package]]\nname = "{component["name"]}"\nversion = "0.1.0"\n'
            (root / "Cargo.lock").write_text(lock)
            metadata["target_directory"] = str(root / "target")
            metadata["resolve"] = {"nodes": [
                {"id": p["id"], "deps": []} for p in metadata["packages"]
            ]}
            binary = root / "target" / target / "release" / "fab-rs"
            binary.parent.mkdir(parents=True)
            binary.write_bytes(executable(target))
            (repository / "LICENSE").write_text("Synthetic product license")
            (root / "packaging").mkdir()
            (root / "packaging/README.txt").write_text("Synthetic prerequisites")
            output = root / "target" / "preview"

            def fake_command(*args: str) -> str:
                return "a" * 40 if args[-1] == "HEAD" else ""

            with patch.object(package, "ROOT", root), \
                 patch.object(package, "REPOSITORY", repository), \
                 patch.object(package, "evidence", return_value={"target": target, "toolchain": "1.99.1"}), \
                 patch.object(package, "command", side_effect=fake_command), \
                 patch.object(package.subprocess, "check_output", return_value=json.dumps(metadata)), \
                 patch.object(package, "binary_compatibility", return_value={"minimum_tested_os": None}):
                package.build(target, output)
                package.verify(target, output, False)
                with self.assertRaisesRegex(ValueError, "already exists"):
                    package.build(target, output)
            sbom = (output / f"fab-rs-0.1.0-{target}.cdx.json").read_text()
            self.assertNotIn(str(repository), sbom)
            self.assertEqual(len(json.loads(sbom)["components"]), len(boundaries.MEMBERS))

    def test_actual_host_and_compiler_mismatch_fail_closed(self) -> None:
        with patch.object(native_host.platform, "system", return_value="Darwin"):
            with self.assertRaisesRegex(ValueError, "Native host mismatch"):
                native_host.evidence("x86_64-pc-windows-msvc")
        with patch.object(native_host.platform, "system", return_value="Linux"), \
             patch.object(native_host.platform, "machine", return_value="x86_64"), \
             patch.object(native_host, "command", return_value="release: 1.99.1\nhost: aarch64-apple-darwin"):
            with self.assertRaisesRegex(ValueError, "rustc host/release"):
                native_host.evidence("x86_64-unknown-linux-gnu")
        with patch.object(native_host.platform, "system", return_value="Darwin"), \
             patch.object(native_host.platform, "machine", return_value="x86_64"), \
             patch.object(native_host, "command", return_value="1"):
            with self.assertRaisesRegex(ValueError, "Translated macOS"):
                native_host.evidence("x86_64-apple-darwin")
        with patch.object(native_host.platform, "system", return_value="Windows"), \
             patch.object(native_host.platform, "machine", return_value="AMD64"), \
             patch.dict(os.environ, {"PROCESSOR_ARCHITEW6432": "ARM64"}):
            with self.assertRaisesRegex(ValueError, "Emulated Windows"):
                native_host.evidence("x86_64-pc-windows-msvc")

    def test_smoke_isolates_home_environment_and_preserves_failure_contract(self) -> None:
        calls = []

        def fake_run(args: list[str], **kwargs) -> subprocess.CompletedProcess:
            calls.append((args, kwargs))
            self.assertEqual(kwargs["env"]["PATH"], "")
            self.assertEqual(kwargs["env"]["HOME"], kwargs["cwd"])
            self.assertNotIn("AZURE_FAKE_TEST", kwargs["env"])
            if args[1:] == ["--version"]:
                return subprocess.CompletedProcess(args, 0, "fab-rs 0.1.0\n", "")
            if "--help" in args:
                return subprocess.CompletedProcess(args, 0, "Help text\n", "")
            return subprocess.CompletedProcess(args, 4, '{"status":"Failure"}\n', "")

        with patch.dict(os.environ, {"AZURE_FAKE_TEST": "synthetic"}), \
             patch.object(package.subprocess, "run", side_effect=fake_run):
            package.smoke(Path("/synthetic/fab-rs"), "0.1.0")
        self.assertEqual(len(calls), 9)


if __name__ == "__main__":
    unittest.main()
