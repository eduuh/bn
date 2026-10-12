#!/usr/bin/env python3
"""Load the shipped plugin settings and exercise a real fingers hint selection."""

import fcntl
import os
from pathlib import Path
import pty
import re
import select
import shutil
import struct
import subprocess
import tempfile
import termios
import time
import unittest


REPO = Path(__file__).resolve().parents[1]
URL = "https://example.test/plugin-check"
PLUGINS = ("extrakto", "tmux-fingers", "tmux-open", "tmux-logging")


class PluginsTest(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.TemporaryDirectory(prefix="bn-tmux-plugins-")
        self.addCleanup(self.root.cleanup)
        self.directory = Path(self.root.name)
        anchor = self.directory / ".config/bn"
        anchor.mkdir(parents=True)
        (anchor / "repo").symlink_to(REPO, target_is_directory=True)
        self.socket = str(self.directory / "socket")
        self.env = dict(
            os.environ, HOME=str(self.directory), TERM="xterm-256color",
            XDG_STATE_HOME=str(self.directory / "state"),
            XDG_CONFIG_HOME=str(self.directory / ".config"),
            XDG_CACHE_HOME=str(self.directory / "cache"),
        )
        self.env.pop("TMUX", None)
        self.env.pop("TMUX_PANE", None)
        self.binary = shutil.which("tmux")
        self.fingers = shutil.which("tmux-fingers")
        self.master, self.slave = pty.openpty()
        self.addCleanup(os.close, self.master)
        self.addCleanup(os.close, self.slave)
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        self.addCleanup(self.stop_server)
        self.pane = self.tmux(
            "-f", "/dev/null", "new-session", "-d", "-s", "test",
            "-P", "-F", "#{pane_id}", f"printf '%s\\n' {URL}; exec sh",
        ).strip()
        self.window = self.tmux("display-message", "-p", "-t", self.pane, "#{window_id}").strip()
        pid = self.tmux("display-message", "-p", "#{pid}").strip()
        self.env["TMUX"] = f"{self.socket},{pid},0"
        self.tmux("set-option", "-g", "status", "off")
        self.tmux("set-option", "-g", "set-clipboard", "off")
        self.tmux("bind-key", "Tab", "display-message", "existing attention binding")
        self.tmux("bind-key", "P", "choose-buffer")
        configuration = []
        for line in (REPO / "config/workflow/tmux.conf").read_text().splitlines():
            if line.startswith("set -g @") and re.search(
                r"@(extrakto_|fingers-|open|logging|screen-capture|save-complete-history)", line,
            ):
                configuration.append(line)
            elif line.startswith("run-shell ") and any(f"/plugins/{name}/" in line for name in PLUGINS):
                configuration.append(line)
            elif line.startswith('bind -N "Text tools'):
                configuration.append(line)
        self.tmux("source-file", "-", input="\n".join(configuration) + "\n")
        self.client = subprocess.Popen(
            [self.binary, "-S", self.socket, "attach-session", "-t", "test"],
            env=self.env, stdin=self.slave, stdout=self.slave, stderr=self.slave,
        )
        self.addCleanup(self.stop_process, self.client)
        self.wait_for(lambda: bool(self.tmux("list-clients").strip()), "attached tmux client")

    def stop_server(self):
        subprocess.run(
            [self.binary, "-S", self.socket, "kill-server"],
            env=self.env, capture_output=True, timeout=5,
        )

    def stop_process(self, process):
        if process.poll() is None:
            process.terminate()
        process.communicate(timeout=5)

    def tmux(self, *args, input=None):
        result = subprocess.run(
            [self.binary, "-S", self.socket, *args], env=self.env, input=input,
            capture_output=True, text=True, timeout=5,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def wait_for(self, condition, description):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if select.select([self.master], [], [], 0.05)[0]:
                os.read(self.master, 65536)
            if condition():
                return
        self.fail(f"timed out waiting for {description}")

    def test_plugins_load_without_replacing_existing_bindings(self):
        prefix = self.tmux("list-keys", "-T", "prefix")
        copy = self.tmux("list-keys", "-T", "copy-mode-vi")
        self.assertRegex(prefix, r"\bTab\s+display-message \"existing attention binding\"")
        self.assertRegex(prefix, r"\bP\s+choose-buffer")
        self.assertRegex(copy, r"\bo\s+send-keys -X other-end")
        for key in ("u", "F", "Y", "C-y", "H"):
            self.assertRegex(prefix, rf"\s{re.escape(key)}\s")
        for key in ("M-o", "M-e", "M-s"):
            self.assertRegex(copy, rf"\s{re.escape(key)}\s")
        self.assertEqual(self.tmux("show-option", "-gqv", "@extrakto_clip_mode").strip(), "tmux_osc52")
        self.assertEqual(self.tmux("show-option", "-gqv", "@fingers-cli").strip(), self.fingers)
        self.assertEqual(self.tmux("display-message", "-p", "-t", self.pane, "#{pane_pipe}").strip(), "0")
        self.assertFalse((self.directory / ".local/state/bn/tmux-logs").exists())

    def test_fingers_copies_a_visible_url_and_restores_the_pane(self):
        process = subprocess.Popen(
            [self.fingers, "start", self.pane, "--patterns", "url"],
            env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        self.addCleanup(self.stop_process, process)
        self.wait_for(
            lambda: self.tmux("display-message", "-p", "#{client_key_table}").strip() == "fingers",
            "fingers hint mode",
        )
        rendered = self.tmux("capture-pane", "-p", "-t", self.window)
        hint = re.search(r"([a-z])ttps://example\.test/plugin-check", rendered)
        self.assertIsNotNone(hint, rendered)
        os.write(self.master, hint.group(1).encode())
        stdout, stderr = process.communicate(timeout=5)
        self.assertEqual(process.returncode, 0, (stdout + stderr).decode())
        self.assertEqual(self.tmux("show-buffer").strip(), URL)
        self.assertEqual(self.tmux("display-message", "-p", "#{client_key_table}").strip(), "root")
        self.assertEqual(self.tmux("list-panes", "-t", self.window, "-F", "#{pane_id}").strip(), self.pane)


if __name__ == "__main__":
    unittest.main()
