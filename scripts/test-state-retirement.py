#!/usr/bin/env python3
"""Regression cases for shipped workflows that no longer use session notes."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


class StateRetirementTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name)
        self.bin = self.home / "bin"
        self.bin.mkdir()
        self.calls = self.home / "calls.jsonl"
        self.env = {
            **os.environ,
            "HOME": str(self.home),
            "PATH": f"{self.bin}:/usr/bin:/bin",
            "CALLS": str(self.calls),
            "BN_NO_AUTO_TICK": "1",
            "BN_NO_CATCHUP": "1",
        }
        self.mock("workspan", """#!/usr/bin/env python3
import json, os, sys
with open(os.environ["CALLS"], "a") as out:
    out.write(json.dumps({"args": sys.argv[1:],
                          "input": sys.stdin.read() if "ingest" in sys.argv else ""}) + "\\n")
print("imported")
""")
        self.env["BN"] = str(self.bin / "workspan")

    def mock(self, name, body):
        path = self.bin / name
        path.write_text(body)
        path.chmod(0o755)
        return path

    def recorded(self):
        return [json.loads(line) for line in self.calls.read_text().splitlines()]

    def test_lane_supplies_complete_checklist_before_launch(self):
        checklist = "#12 [red] fix quoted 'title'\n#14 [conflict] preserve both sides"
        proc = subprocess.run(
            ["sh", "-c",
             '. "$1"; LANE=red-main; BN_RED_MAIN_AGENT=claude; '
             'lane_dispatch demo lane red-main "repair CI" "$2"',
             "sh", str(ROOT / "config/jobs/lib/lanes.sh"), checklist],
            env=self.env, cwd=self.home, capture_output=True, text=True,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        calls = self.recorded()
        self.assertEqual(len(calls), 1)
        args = calls[0]["args"]
        self.assertEqual(args[0], "dispatch")
        self.assertIn("claude", args)
        self.assertIn(checklist, args[-1])
        self.assertIn("--- BEGIN CHECKLIST ---", args[-1])

    def test_ado_import_uses_task_provider_with_stable_keys(self):
        self.mock("az", """#!/bin/sh
printf '%s\\n' '[{"id":12,"fields":{"System.Title":"Fix quotes and spaces"}}]'
""")
        command = ["bash", str(ROOT / "config/scripts/pull-ado.sh"), "--repo", "demo"]
        for _ in range(2):
            proc = subprocess.run(command, env=self.env, cwd=self.home,
                                  capture_output=True, text=True)
            self.assertEqual(proc.returncode, 0, proc.stderr)
        calls = self.recorded()
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[0], calls[1])
        self.assertEqual(calls[0]["args"][:2], ["task", "ingest"])
        self.assertEqual(json.loads(calls[0]["input"]), [
            {"text": "[#12] Fix quotes and spaces repo:demo", "key": "demo:12"}
        ])
        self.assertIn("--source", calls[0]["args"])

    def test_ado_import_reports_provider_failure(self):
        self.mock("az", """#!/bin/sh
printf '%s\\n' '[{"id":12,"fields":{"System.Title":"Fix CI"}}]'
""")
        self.mock("workspan", "#!/bin/sh\nexit 1\n")
        proc = subprocess.run(
            ["bash", str(ROOT / "config/scripts/pull-ado.sh"), "--repo", "demo"],
            env=self.env, cwd=self.home, capture_output=True, text=True,
        )
        self.assertEqual(proc.returncode, 5)
        self.assertIn("task import failed", proc.stderr)

    def test_statusline_does_not_invoke_note_runtime(self):
        repo = self.home / "demo"
        repo.mkdir()
        subprocess.run(["git", "init", "-q", "-b", "main", str(repo)],
                       env=self.env, check=True)
        self.mock("bn", "#!/bin/sh\nexit 99\n")
        proc = subprocess.run(
            ["bash", str(ROOT / "config/workflow/bin/claude-statusline.sh")],
            input='{"model":{"display_name":"Model"}}', env=self.env, cwd=repo,
            capture_output=True, text=True,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertTrue(proc.stdout.endswith("demo:main"), proc.stdout)
        self.assertFalse(self.calls.exists())

    def test_prompt_guard_rejects_retired_calls_but_allows_task_workflows(self):
        prompts = self.home / "config/prompts"
        prompts.mkdir(parents=True)
        prompt = prompts / "start-session.md"
        command = ["sh", str(ROOT / "scripts/check-prompt-sources.sh")]
        for retired in ("bn_context", "bn --cat", "workspan add todo"):
            with self.subTest(retired=retired):
                prompt.write_text(f"Run `{retired}`.\n")
                proc = subprocess.run(command, env=self.env, cwd=self.home,
                                      capture_output=True, text=True)
                self.assertNotEqual(proc.returncode, 0)
                self.assertIn("Retired session-note instructions", proc.stderr)
        prompt.write_text("Run `workspan task --repo .` and `entire status`.\n")
        proc = subprocess.run(command, env=self.env, cwd=self.home,
                              capture_output=True, text=True)
        self.assertEqual(proc.returncode, 0, proc.stderr)


if __name__ == "__main__":
    unittest.main()
