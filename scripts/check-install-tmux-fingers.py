#!/usr/bin/env python3
"""Verify the pinned fingers installer without network access."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


INSTALLER = Path(__file__).with_name("install-tmux-fingers.sh")


class FingersInstallTest(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.TemporaryDirectory(prefix="bn-fingers-install-")
        self.addCleanup(self.root.cleanup)
        self.directory = Path(self.root.name)
        self.shims = self.directory / "shims"
        self.shims.mkdir()
        self.destination = self.directory / "bin with spaces"
        self.destination.mkdir()
        self.env = dict(os.environ, PATH=f"{self.shims}:{os.environ['PATH']}")
        self.shim("uname", 'case "$1" in -s) echo Linux;; -m) echo x86_64;; esac')
        self.shim("curl", 'echo unexpected network call >&2; exit 99')

    def shim(self, name, body):
        file = self.shims / name
        file.write_text("#!/bin/sh\n" + body + "\n")
        file.chmod(0o755)

    def install(self):
        return subprocess.run(
            ["bash", str(INSTALLER), str(self.destination)],
            env=self.env, capture_output=True, text=True, timeout=5,
        )

    def test_matching_version_requires_no_download(self):
        binary = self.destination / "tmux-fingers"
        binary.write_text("#!/bin/sh\necho 2.7.1\n")
        binary.chmod(0o755)
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("already installed", result.stdout)
        self.assertNotIn("network", result.stderr)

    def test_bad_checksum_does_not_replace_existing_binary(self):
        binary = self.destination / "tmux-fingers"
        original = "#!/bin/sh\necho 2.6.0\n"
        binary.write_text(original)
        binary.chmod(0o755)
        self.shim("curl", 'while [ "$#" -gt 0 ]; do if [ "$1" = -o ]; then printf invalid > "$2"; exit 0; fi; shift; done; exit 1')
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("checksum mismatch", result.stderr)
        self.assertEqual(binary.read_text(), original)
        self.assertEqual(list(self.destination.glob(".tmux-fingers.*")), [])

    def test_unsupported_platform_reports_error_without_download(self):
        self.shim("uname", 'case "$1" in -s) echo Linux;; -m) echo aarch64;; esac')
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no pinned binary", result.stderr)
        self.assertNotIn("network", result.stderr)
        self.assertFalse((self.destination / "tmux-fingers").exists())

    def test_full_installer_respects_core_mode_and_dependency_opt_out(self):
        installer = INSTALLER.parents[1] / "install.sh"
        text = installer.read_text()
        phase = text.split('install_required_cli_deps "$mode"\n', 1)[1]
        phase = phase.split("# --- shell CLIs:", 1)[0]
        fixture = self.directory / "phase.sh"
        fixture.write_text(phase)
        command = """
bn_install_ensure_tmux() { :; }
bn_install_ensure_tmux_fingers() { echo FINGERS_INSTALLED; }
mode="$1"
BN_INSTALL_DEPS="$2"
source "$3"
"""
        for mode, deps, expected in (
            ("full", "1", "FINGERS_INSTALLED"),
            ("core", "1", ""),
            ("full", "0", ""),
        ):
            with self.subTest(mode=mode, deps=deps):
                result = subprocess.run(
                    ["bash", "-c", command, "test", mode, deps, str(fixture)],
                    env=self.env, capture_output=True, text=True, timeout=5,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(), expected)


if __name__ == "__main__":
    unittest.main()
