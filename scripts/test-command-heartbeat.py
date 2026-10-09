#!/usr/bin/env python3
"""Regression tests for the shipped foreground command wrapper."""
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
RUNNER = ROOT / "config" / "cmds" / "run.sh"


@unittest.skipUnless(os.name == "posix", "bn commands require a POSIX shell")
class HeartbeatTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="bn-heartbeat-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.log = self.root / "command.log"
        self.env = {**os.environ, "XDG_CACHE_HOME": str(self.root / "cache")}

    def run_command(self, code, *arguments, options=(), stdin=None):
        return subprocess.run(
            [
                "/bin/sh", str(RUNNER), "--log", str(self.log),
                *options, "--", sys.executable, "-c", code, *arguments,
            ],
            input=stdin,
            capture_output=True,
            env=self.env,
            timeout=8,
        )

    def test_silent_command_reports_heartbeat_inactivity_and_result(self):
        result = self.run_command(
            "import time; time.sleep(0.25)",
            options=("--label", "quiet test", "--interval", "0.04", "--idle-after", "0.09"),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, b"")
        self.assertIn(b"Starting: quiet test", result.stderr)
        self.assertIn(b"Still running: quiet test", result.stderr)
        self.assertIn(b"WARNING: no output for", result.stderr)
        self.assertIn(b"Finished: quiet test", result.stderr)
        self.assertIn(b"exit 0", result.stderr)
        self.assertIn(str(self.log).encode(), result.stderr)
        self.assertEqual(self.log.read_bytes(), b"")

    def test_streams_and_logs_output_without_changing_failure_status(self):
        result = self.run_command(
            "import sys; print('out', flush=True); print('err', file=sys.stderr, flush=True); sys.exit(37)"
        )
        self.assertEqual(result.returncode, 37)
        self.assertEqual(result.stdout, b"out\n")
        self.assertIn(b"err\n", result.stderr)
        self.assertIn(b"exit 37", result.stderr)
        self.assertIn(b"out\n", self.log.read_bytes())
        self.assertIn(b"err\n", self.log.read_bytes())
        self.assertNotIn(b"Starting:", self.log.read_bytes())

    def test_regular_output_resets_inactivity(self):
        result = self.run_command(
            "import time\nfor i in range(6):\n print(i, flush=True)\n time.sleep(0.15)",
            options=("--interval", "0.1", "--idle-after", "0.45"),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(b"Still running:", result.stderr)
        self.assertNotIn(b"WARNING: no output", result.stderr)
        self.assertEqual(result.stdout, b"0\n1\n2\n3\n4\n5\n")

    def test_large_output_is_not_lost(self):
        result = self.run_command(
            "import sys; sys.stdout.write('x' * 300000); sys.stderr.write('y' * 300000)"
        )
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, b"x" * 300000)
        self.assertIn(b"y" * 300000, result.stderr)
        log = self.log.read_bytes()
        self.assertEqual(log.count(b"x"), 300000)
        self.assertEqual(log.count(b"y"), 300000)

    def test_arguments_are_preserved_without_shell_interpolation(self):
        arguments = ("two words", "", "$(printf literal)", "--flag")
        result = self.run_command(
            "import json,sys; print(json.dumps(sys.argv[1:]))", *arguments
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), list(arguments))

    def test_stdin_is_preserved(self):
        result = self.run_command(
            "import sys; sys.stdout.write(sys.stdin.read())", stdin=b"hello\n"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, b"hello\n")

    def test_missing_executable_is_explicit(self):
        result = subprocess.run(
            ["/bin/sh", str(RUNNER), "--log", str(self.log), "--", str(self.root / "missing")],
            capture_output=True, env=self.env, timeout=8,
        )
        self.assertEqual(result.returncode, 127)
        self.assertIn(b"could not start", result.stderr)
        self.assertEqual(result.stdout, b"")

    def test_existing_log_is_not_overwritten(self):
        self.log.write_bytes(b"keep me")
        result = self.run_command("print('must not run')")
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, b"")
        self.assertEqual(self.log.read_bytes(), b"keep me")
        self.assertIn(b"run:", result.stderr)

    def test_non_executable_command_returns_permission_failure(self):
        command = self.root / "not-executable"
        command.write_text("#!/bin/sh\nexit 0\n")
        result = subprocess.run(
            ["/bin/sh", str(RUNNER), "--log", str(self.log), "--", str(command)],
            capture_output=True, env=self.env, timeout=8,
        )
        self.assertEqual(result.returncode, 126)
        self.assertIn(b"could not start", result.stderr)
        self.assertIn(b"exit 126", result.stderr)

    def test_invalid_timing_is_rejected_before_execution(self):
        for option in ("--interval", "--idle-after"):
            for value in ("0", "-1", "nan", "inf", "bad"):
                with self.subTest(option=option, value=value):
                    result = self.run_command("print('must not run')", options=(option, value))
                    self.assertEqual(result.returncode, 2)
                    self.assertEqual(result.stdout, b"")
                    self.assertIn(b"positive finite", result.stderr)
        self.assertFalse(self.log.exists())

    def test_missing_command_is_rejected(self):
        result = subprocess.run(
            ["/bin/sh", str(RUNNER)],
            capture_output=True, env=self.env, timeout=8,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn(b"command", result.stderr)

    def test_default_log_is_private_and_outside_the_repository(self):
        result = subprocess.run(
            ["/bin/sh", str(RUNNER), "--", sys.executable, "-c", "print('ok')"],
            capture_output=True, env=self.env, timeout=8,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        logs = list((self.root / "cache" / "bn" / "commands").glob("*.log"))
        self.assertEqual(len(logs), 1)
        self.assertEqual(logs[0].read_bytes(), b"ok\n")
        self.assertEqual(logs[0].stat().st_mode & 0o777, 0o600)

    def test_signal_is_forwarded_and_exit_status_preserved(self):
        with subprocess.Popen(
            [
                "/bin/sh", str(RUNNER), "--log", str(self.log),
                "--interval", "0.05", "--idle-after", "0.2", "--",
                sys.executable, "-c",
                "import os,time; print(os.getpid(), flush=True); time.sleep(100)",
            ],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=self.env,
        ) as process:
            try:
                ready, _, _ = select.select([process.stdout], [], [], 5)
                self.assertTrue(ready, "wrapped command did not start")
                child_pid = int(process.stdout.readline())
                for expected in (b"Starting:", b"Still running:"):
                    ready, _, _ = select.select([process.stderr], [], [], 5)
                    self.assertTrue(ready, "live status notification did not arrive")
                    self.assertIn(expected, process.stderr.readline())
                self.assertIsNone(process.poll(), "heartbeat arrived only after completion")
                process.send_signal(signal.SIGTERM)
                _, stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 143, stderr)
                self.assertIn(b"exit 143", stderr)
                with self.assertRaises(ProcessLookupError):
                    os.kill(child_pid, 0)
            finally:
                if process.poll() is None:
                    process.terminate()
                    process.communicate(timeout=5)


if __name__ == "__main__":
    unittest.main()
