#!/usr/bin/env python3
"""Exercise Codespace commands with fake gh/ssh and a private SSH config."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


class CodespaceCliTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.ssh = root / ".ssh"
        self.ssh.mkdir()
        self.config = self.ssh / "config"
        self.calls = root / "calls"
        binaries = root / "bin"
        binaries.mkdir()
        gh = binaries / "gh"
        gh.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, pathlib, sys\n"
            "with pathlib.Path(os.environ['BN_TEST_CALLS']).open('a') as log:\n"
            "    log.write(json.dumps(sys.argv[1:]) + '\\n')\n"
            "if os.environ.get('BN_TEST_GH_FAIL'):\n"
            "    sys.exit('fixture GitHub failure')\n"
            "if sys.argv[1:3] != ['cs', 'list']:\n"
            "    sys.exit('stopped codespace must not be started')\n"
            "print(json.dumps([{'name': 'fixture-codespace', "
            "'displayName': 'fixture umbrella', 'state': 'Shutdown'}]))\n"
        )
        gh.chmod(0o700)
        ssh = binaries / "ssh"
        ssh.write_text("#!/bin/sh\nexit 99\n")
        ssh.chmod(0o700)
        self.env = {
            **os.environ,
            "HOME": str(root),
            "BN_SSH_DIR": str(self.ssh),
            "BN_TEST_CALLS": str(self.calls),
            "GH_CONFIG_DIR": str(root / "gh"),
            "GH_TOKEN": "fixture",
            "PATH": str(binaries) + os.pathsep + os.environ["PATH"],
        }

    def connect(self):
        return subprocess.run(
            [ROOT / "target/debug/bn", "cs", "connect", "fixture-codespace"],
            env=self.env,
            capture_output=True,
            text=True,
            timeout=10,
        )

    def test_stopped_codespace_only_lists_and_preserves_other_hosts(self):
        original = "Host home\n  HostName 10.0.0.2\n"
        self.config.write_text(original)
        result = self.connect()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(self.config.read_text().startswith(original))
        self.assertIn("# >>> bn codespace cs-umbrella >>>", self.config.read_text())
        self.assertEqual(
            [json.loads(line) for line in self.calls.read_text().splitlines()],
            [["cs", "list", "--json", "name,displayName,state"]],
        )
        self.assertEqual((self.ssh / "config.bak-bn-cs").read_text(), original)

    def test_unreadable_config_is_not_overwritten(self):
        original = b"\xfforiginal SSH config"
        self.config.write_bytes(original)
        result = self.connect()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.config.read_bytes(), original)
        self.assertIn("could not read", result.stderr)

    def test_invalid_backup_preserves_original_config(self):
        original = "Host home\n  HostName 10.0.0.2\n"
        self.config.write_text(original)
        (self.ssh / "config.bak-bn-cs").mkdir()
        result = self.connect()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.config.read_text(), original)
        self.assertIn("backup", result.stderr)

    def test_github_failure_keeps_its_actual_error(self):
        self.env["BN_TEST_GH_FAIL"] = "1"
        result = self.connect()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("fixture GitHub failure", result.stderr)
        self.assertFalse(self.config.exists())


if __name__ == "__main__":
    unittest.main()
