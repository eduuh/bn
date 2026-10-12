#!/usr/bin/env python3
"""Verify opt-in, private, pane-targeted logging on an isolated tmux server."""

import os
from pathlib import Path
import shlex
import shutil
import stat
import subprocess
import tempfile
import time
import unittest


PLUGIN = Path(__file__).resolve().parents[1] / "config/workflow/plugins/tmux-logging"


class LoggingTest(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.TemporaryDirectory(prefix="bn-logging-")
        self.addCleanup(self.root.cleanup)
        self.directory = Path(self.root.name)
        self.socket = str(self.directory / "socket")
        self.logs = self.directory / "private logs"
        self.env = dict(os.environ, HOME=str(self.directory), TERM="xterm-256color")
        self.env.pop("TMUX", None)
        self.env.pop("TMUX_PANE", None)
        self.binary = shutil.which("tmux")
        self.addCleanup(self.stop_server)
        self.pane = self.tmux(
            "-f", "/dev/null", "new-session", "-d", "-s", "test",
            "-P", "-F", "#{pane_id}", "sh",
        ).strip()
        pid = self.tmux("display-message", "-p", "#{pid}").strip()
        self.env["TMUX"] = f"{self.socket},{pid},0"
        for option in ("@logging-path", "@screen-capture-path", "@save-complete-history-path"):
            self.tmux("set-option", "-g", option, str(self.logs))

    def stop_server(self):
        subprocess.run(
            [self.binary, "-S", self.socket, "kill-server"],
            env=self.env, capture_output=True, timeout=5,
        )

    def tmux(self, *args):
        return subprocess.run(
            [self.binary, "-S", self.socket, *args], env=self.env,
            check=True, capture_output=True, text=True, timeout=5,
        ).stdout

    def script(self, name, pane=None, check=True):
        return subprocess.run(
            ["bash", str(PLUGIN / "scripts" / name), pane or self.pane],
            env=self.env, check=check, capture_output=True, text=True, timeout=5,
        )

    def output(self, text, pane=None):
        target = pane or self.pane
        self.tmux("send-keys", "-t", target, "-l", f"printf '%s\\n' {shlex.quote(text)}")
        self.tmux("send-keys", "-t", target, "Enter")
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if text in self.tmux("capture-pane", "-p", "-t", target):
                return
            time.sleep(0.05)
        self.fail("pane did not print the test marker")

    def assert_private(self, file):
        self.assertEqual(stat.S_IMODE(self.logs.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(file.stat().st_mode), 0o600)

    def test_logging_is_opt_in_and_stops(self):
        self.tmux("run-shell", f"bash {shlex.quote(str(PLUGIN / 'logging.tmux'))}")
        self.assertEqual(self.tmux("display-message", "-p", "-t", self.pane, "#{pane_pipe}").strip(), "0")
        self.assertFalse(self.logs.exists())
        self.script("toggle_logging.sh")
        self.output("RECORDED_OUTPUT")
        self.script("toggle_logging.sh")
        self.output("AFTER_LOGGING_STOPPED")
        files = list(self.logs.glob("tmux-*.log*"))
        self.assertEqual(len(files), 1)
        deadline = time.monotonic() + 5
        while "RECORDED_OUTPUT" not in files[0].read_text() and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertIn("RECORDED_OUTPUT", files[0].read_text())
        self.assertNotIn("AFTER_LOGGING_STOPPED", files[0].read_text())
        self.assert_private(files[0])

    def test_snapshots_target_requested_pane_not_focused_pane(self):
        self.output("REQUESTED_PANE")
        other = self.tmux("split-window", "-P", "-F", "#{pane_id}", "sh").strip()
        self.output("OTHER_PANE", other)
        self.script("screen_capture.sh")
        self.script("save_complete_history.sh")
        files = list(self.logs.glob("*.log*"))
        self.assertEqual(len(files), 2)
        for file in files:
            self.assertIn("REQUESTED_PANE", file.read_text())
            self.assertNotIn("OTHER_PANE", file.read_text())
            self.assert_private(file)

    def test_unrelated_pane_pipe_is_not_replaced(self):
        self.tmux("pipe-pane", "-t", self.pane, "cat >/dev/null")
        result = self.script("toggle_logging.sh", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("already", result.stderr)
        self.assertEqual(self.tmux("display-message", "-p", "-t", self.pane, "#{pane_pipe}").strip(), "1")

    def test_history_export_includes_text_no_longer_visible(self):
        self.output("EARLY_HISTORY_MARKER")
        self.tmux("send-keys", "-t", self.pane, "-l", 'i=1; while [ "$i" -le 80 ]; do echo "$i"; i=$((i + 1)); done')
        self.tmux("send-keys", "-t", self.pane, "Enter")
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            screen = self.tmux("capture-pane", "-p", "-t", self.pane)
            if "80" in screen and "EARLY_HISTORY_MARKER" not in screen:
                break
            time.sleep(0.05)
        self.script("screen_capture.sh")
        self.script("save_complete_history.sh")
        screen_file = next(self.logs.glob("tmux-screen-capture-*.log*"))
        history_file = next(self.logs.glob("tmux-history-*.log*"))
        self.assertNotIn("EARLY_HISTORY_MARKER", screen_file.read_text())
        self.assertIn("EARLY_HISTORY_MARKER", history_file.read_text())
        self.assert_private(history_file)


if __name__ == "__main__":
    unittest.main()
