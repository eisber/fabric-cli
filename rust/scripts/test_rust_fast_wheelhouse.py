# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Offline tests for exact pin and public wheel provenance enforcement."""

import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest
import zipfile

SPEC = importlib.util.spec_from_file_location(
    "wheelhouse", Path(__file__).with_name("rust-fast-wheelhouse.py")
)
assert SPEC and SPEC.loader
WHEELHOUSE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WHEELHOUSE)


class WheelhouseTests(unittest.TestCase):
    def test_exact_pins_reject_duplicates_ranges_and_commands(self) -> None:
        self.assertEqual(WHEELHOUSE.pins("PyJWT==2.15.1\n"), {"pyjwt": "2.15.1"})
        for text in ("name>=1", "--trusted-host x", "name==1\nName==1", "x==1;echo"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                WHEELHOUSE.pins(text)
        self.assertEqual(len(WHEELHOUSE.pins(WHEELHOUSE.CONSTRAINTS.read_text())), 28)

    def test_public_hash_size_yanked_and_origin_are_mandatory(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "test.whl"
            path.write_bytes(b"synthetic wheel")
            entry = {
                "filename": path.name,
                "yanked": False,
                "size": path.stat().st_size,
                "url": "https://files.pythonhosted.org/packages/test.whl",
                "digests": {"sha256": hashlib.sha256(path.read_bytes()).hexdigest()},
            }
            self.assertEqual(
                WHEELHOUSE.verify_public_wheel(path, {"urls": [entry]})["sha256"],
                WHEELHOUSE.file_hash(path),
            )
            for change in (
                {"yanked": True},
                {"size": 0},
                {"url": "http://example.test/test.whl"},
                {"digests": {"sha256": "0" * 64}},
            ):
                with self.subTest(change=change), self.assertRaises(ValueError):
                    WHEELHOUSE.verify_public_wheel(
                        path, {"urls": [{**entry, **change}]}
                    )

    def test_product_source_content_and_missing_files_are_checked(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            product = source / "src" / "fabric_cli"
            product.mkdir(parents=True)
            (product / "__init__.py").write_bytes(b"original")
            wheel = source / "product.whl"
            with zipfile.ZipFile(wheel, "w") as archive:
                archive.writestr("fabric_cli/__init__.py", b"original")
            self.assertEqual(WHEELHOUSE.verify_product_wheel(wheel, source), 1)
            (product / "missing.yaml").write_bytes(b"required")
            with self.assertRaisesRegex(ValueError, "Missing packaged"):
                WHEELHOUSE.verify_product_wheel(wheel, source)
            (product / "__init__.py").write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "Product/source mismatch"):
                WHEELHOUSE.verify_product_wheel(wheel, source)


if __name__ == "__main__":
    unittest.main()
