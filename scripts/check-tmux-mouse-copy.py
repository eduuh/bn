#!/usr/bin/env python3
"""Exercise workflow mouse-copy bindings through an isolated tmux client's PTY."""

import fcntl
import os
from pathlib import Path
import pty
import select
import shlex
import struct
import subprocess
import tempfile
import termios
import time
import unittest


CONFIG = Path(__file__).resolve().parents[1] / "config/workflow/tmux.conf"
MARKER = "MOUSE_COPY_MARKER"


class MouseCopyTest(unittest.TestCase):
    def check_drag(self, application_mouse, already_in_copy_mode=False):
        with tempfile.TemporaryDirectory(prefix="bn-mouse-copy-") as directory:
            socket = str(Path(directory) / "socket")
            clipboard = Path(directory) / "clipboard"
            env = dict(os.environ, TERM="xterm-256color")
            env.pop("TMUX", None)
            env.pop("TMUX_PANE", None)

            def tmux(*args, input=None):
                return subprocess.run(
                    ["tmux", "-S", socket, *args],
                    env=env,
                    check=True,
                    capture_output=True,
                    text=True,
                    input=input,
                    timeout=5,
                ).stdout.strip()

            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
            client = None
            try:
                mouse = r"\033[?1000h\033[?1006h" if application_mouse else ""
                command = (
                    "stty raw -echo; printf '"
                    + mouse
                    + MARKER
                    + r"\r\n'; cat >/dev/null"
                )
                tmux("-f", "/dev/null", "new-session", "-d", "-s", "test", command)
                tmux("set-option", "-g", "status", "off")
                tmux("set-option", "-g", "mouse", "on")
                tmux("set-option", "-g", "set-clipboard", "off")
                tmux("set-window-option", "-g", "mode-keys", "vi")
                tmux("set-option", "-g", "copy-command", f"cat > {shlex.quote(str(clipboard))}")
                bindings = "\n".join(
                    line for line in CONFIG.read_text().splitlines()
                    if line.startswith("bind ") and "MouseDrag1Pane" in line
                )
                if bindings:
                    tmux("source-file", "-", input=bindings)
                client = subprocess.Popen(
                    ["tmux", "-S", socket, "attach-session", "-t", "test"],
                    env=env,
                    stdin=slave,
                    stdout=slave,
                    stderr=slave,
                )
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    if select.select([master], [], [], 0.05)[0]:
                        os.read(master, 65536)
                    ready = tmux("display-message", "-p", "#{mouse_any_flag}")
                    attached = tmux("list-clients")
                    if attached and ready == str(int(application_mouse)):
                        break
                else:
                    self.fail("tmux client or application mouse reporting did not become ready")
                if already_in_copy_mode:
                    tmux("copy-mode")
                for event in (
                    b"\033[<0;1;1M",
                    f"\033[<32;{len(MARKER) + 1};1M".encode(),
                    f"\033[<0;{len(MARKER) + 1};1m".encode(),
                ):
                    os.write(master, event)
                    time.sleep(0.1)
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    if clipboard.exists() and clipboard.read_text().rstrip("\n") == MARKER:
                        break
                    time.sleep(0.05)
                self.assertTrue(clipboard.exists(), "mouse release did not invoke copy-command")
                self.assertEqual(clipboard.read_text().rstrip("\n"), MARKER)
                self.assertEqual(tmux("show-buffer").rstrip("\n"), MARKER)
            finally:
                if client is not None:
                    client.terminate()
                    client.wait(timeout=5)
                subprocess.run(
                    ["tmux", "-S", socket, "kill-server"], env=env,
                    capture_output=True, timeout=5,
                )
                os.close(master)
                os.close(slave)

    def test_drag_copies_when_application_requests_mouse(self):
        self.check_drag(True)

    def test_drag_copies_in_plain_shell(self):
        self.check_drag(False)

    def test_drag_still_copies_inside_copy_mode(self):
        self.check_drag(True, already_in_copy_mode=True)


if __name__ == "__main__":
    unittest.main()
