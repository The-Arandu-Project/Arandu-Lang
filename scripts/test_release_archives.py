#!/usr/bin/env python3
"""Negative regressions for release archive validation."""

import importlib.util
from pathlib import Path
import tarfile
import tempfile
import unittest
import zipfile
import json


def load(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


tar_tools = load("reproducible_tar", "reproducible_tar.py")
zip_tools = load("reproducible_zip", "reproducible_zip.py")


class ArchiveValidationTests(unittest.TestCase):
    def test_tar_rejects_parent_traversal(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "bad.tar.gz"
            with tarfile.open(archive, "w:gz") as output:
                output.addfile(tarfile.TarInfo("../escape"))
            with self.assertRaises(SystemExit):
                tar_tools.validate(archive, "arandu-0.0.1", "x86_64-unknown-linux-gnu", "0.0.1")

    def test_zip_rejects_case_insensitive_duplicate(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "bad.zip"
            with zipfile.ZipFile(archive, "w") as output:
                output.writestr("arandu-0.0.1/bin/arandu.exe", b"one")
                output.writestr("arandu-0.0.1/BIN/ARANDU.EXE", b"two")
            with self.assertRaises(SystemExit):
                zip_tools.validate(archive, "arandu-0.0.1", "0.0.1", "x86_64-pc-windows-msvc")

    def test_zip_rejects_parent_traversal(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "bad.zip"
            with zipfile.ZipFile(archive, "w") as output:
                output.writestr("arandu-0.0.1/../../escape", b"bad")
            with self.assertRaises(SystemExit):
                zip_tools.validate(archive, "arandu-0.0.1", "0.0.1", "x86_64-pc-windows-msvc")

    def test_zip_rejects_incomplete_package(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "incomplete.zip"
            with zipfile.ZipFile(archive, "w") as output:
                output.writestr("arandu-0.0.1/bin/arandu.exe", b"binary")
            with self.assertRaises(SystemExit):
                zip_tools.validate(archive, "arandu-0.0.1", "0.0.1", "x86_64-pc-windows-msvc")

    def test_zip_rejects_manifest_target_mismatch(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "wrong-target.zip"
            root = "arandu-0.0.1"
            required = {
                "bin/arandu.exe": b"a", "bin/arandu_cli.exe": b"a",
                "bin/arandu-lsp.exe": b"l", "BLAKE3SUMS": b"",
                "lib/x86_64-pc-windows-msvc/arandu_runtime.lib": b"r",
                "LICENSE-MIT": b"m", "LICENSE-APACHE": b"a",
                "share/arandu/stdlib/std/io.aru": b"public func print() {}",
            }
            manifest = {"schema": 1, "version": "0.0.1", "target": "aarch64-pc-windows-msvc", "components": ["arandu", "arandu-lsp", "runtime", "stdlib"], "archive": "zip"}
            with zipfile.ZipFile(archive, "w") as output:
                for name, contents in required.items():
                    output.writestr(f"{root}/{name}", contents)
                output.writestr(f"{root}/release-manifest.json", json.dumps(manifest))
            with self.assertRaises(SystemExit):
                zip_tools.validate(archive, root, "0.0.1", "x86_64-pc-windows-msvc")

    def test_zip_rejects_unexpected_content(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "extra.zip"
            with zipfile.ZipFile(archive, "w") as output:
                output.writestr("arandu-0.0.1/autorun.dll", b"unexpected")
            with self.assertRaises(SystemExit):
                zip_tools.validate(archive, "arandu-0.0.1", "0.0.1", "x86_64-pc-windows-msvc")


if __name__ == "__main__":
    unittest.main()
