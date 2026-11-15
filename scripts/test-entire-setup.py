#!/usr/bin/env python3
"""Entire onboarding must choose a verified private destination before enabling capture."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


REPO = Path(__file__).resolve().parents[1]
SCRIPT = REPO / "config/workflow/bin/setup-entire.sh"


class EntireSetupTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name)
        self.project = self.home / "project"
        self.bin = self.home / "bin"
        self.bin.mkdir()
        self.env = {
            **os.environ,
            "HOME": str(self.home),
            "XDG_CONFIG_HOME": str(self.home / ".config"),
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_CONFIG_NOSYSTEM": "1",
            "PATH": f"{self.bin}:{os.environ['PATH']}",
            "BN_ENTIRE_CHECKPOINT_REMOTE": "github:example/checkpoints",
            "CHECKPOINT_PRIVATE": "true",
            "ENABLE_EXIT": "0",
            "CALLS": str(self.home / "calls"),
        }
        subprocess.run(
            ["git", "init", "-q", str(self.project)],
            env=self.env,
            check=True,
        )
        self.fake(
            "gh",
            '#!/bin/sh\nprintf "%s\\n" "$CHECKPOINT_PRIVATE"\n',
        )
        self.fake(
            "entire",
            """#!/usr/bin/env python3
import json, os, pathlib, sys
with open(os.environ["CALLS"], "a") as calls:
    calls.write(json.dumps(sys.argv[1:]) + "\\n")
if sys.argv[1] == "enable":
    if int(os.environ["ENABLE_EXIT"]):
        sys.exit(int(os.environ["ENABLE_EXIT"]))
    path = pathlib.Path(".entire/settings.local.json")
    path.parent.mkdir(exist_ok=True)
    path.write_text(json.dumps({"remote": sys.argv[sys.argv.index("--checkpoint-remote") + 1]}))
""",
        )

    def fake(self, name, content):
        path = self.bin / name
        path.write_text(content)
        path.chmod(0o755)

    def run_setup(self, agent="copilot"):
        return subprocess.run(
            ["bash", str(SCRIPT), agent, str(self.project)],
            env=self.env,
            capture_output=True,
            text=True,
        )

    def calls(self):
        path = self.home / "calls"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def test_private_destination_is_local_and_repeatable(self):
        first = self.run_setup()
        self.assertEqual(first.returncode, 0, first.stderr)
        settings = self.project / ".entire/settings.local.json"
        original = settings.read_bytes()
        second = self.run_setup()
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(settings.read_bytes(), original)
        self.assertEqual(
            self.calls()[0],
            [
                "enable", "--yes", "--no-init-repo", "--local", "--telemetry=false",
                "--agent", "copilot-cli", "--checkpoint-remote", "github:example/checkpoints",
            ],
        )
        self.assertEqual(self.calls()[1], ["status"])
        self.assertFalse((self.project / ".entire/settings.json").exists())

    def test_machine_default_is_used_for_claude(self):
        del self.env["BN_ENTIRE_CHECKPOINT_REMOTE"]
        config = self.home / ".config/bn/entire-checkpoint-remote"
        config.parent.mkdir(parents=True)
        config.write_text("github:example/personal-checkpoints\n")
        result = self.run_setup("claude")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("claude-code", self.calls()[0])
        self.assertEqual(self.calls()[0][-1], "github:example/personal-checkpoints")

    def test_public_destination_never_enables_capture(self):
        self.env["CHECKPOINT_PRIVATE"] = "false"
        result = self.run_setup()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must be private", result.stderr)
        self.assertEqual(self.calls(), [])

    def test_failed_visibility_check_never_enables_capture(self):
        self.fake("gh", "#!/bin/sh\nexit 1\n")
        result = self.run_setup()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not verify", result.stderr)
        self.assertEqual(self.calls(), [])

    def test_unapproved_destination_never_enables_capture(self):
        del self.env["BN_ENTIRE_CHECKPOINT_REMOTE"]
        result = self.run_setup()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("set an approved", result.stderr)
        self.assertEqual(self.calls(), [])

    def test_enable_failure_does_not_fall_back_or_report_status(self):
        self.env["ENABLE_EXIT"] = "9"
        result = self.run_setup()
        self.assertEqual(result.returncode, 9)
        self.assertEqual(len(self.calls()), 1)
        self.assertEqual(self.calls()[0][0], "enable")

    def test_non_git_directory_never_enables_capture(self):
        self.project = self.home
        result = self.run_setup()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("existing Git worktree", result.stderr)
        self.assertEqual(self.calls(), [])


if __name__ == "__main__":
    unittest.main()
