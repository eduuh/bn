#!/usr/bin/env python3
"""Pinned extrakto extraction, scrollback, clipboard, and insertion checks."""

import io
import os
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
PLUGIN = ROOT / "config/workflow/plugins/extrakto"
sys.path.insert(0, str(PLUGIN))
import extrakto_plugin


class FakeFzf:
    def __init__(self, response):
        self.stdin = io.BytesIO()
        self.stdout = io.BytesIO(response.encode())

    def communicate(self, data):
        self.stdin.write(data)
        return self.stdout.getvalue(), None


class RuntimeTests(unittest.TestCase):
    def setUp(self):
        self.calls = []
        self.fzf_response = "\nenter\nTASK-12345\n"
        self.options = {
            "@extrakto_clip_tool": "unused",
            "@extrakto_clip_mode": "tmux_osc52",
            "@extrakto_clip_mode_order": "tmux_osc52 buffer",
            "@extrakto_filter_order": "word path url line",
            "@extrakto_grab_area": "full",
        }
        self.patches = [
            patch.object(extrakto_plugin.subprocess, "check_output", side_effect=self.output),
            patch.object(extrakto_plugin.subprocess, "run", side_effect=self.fake_run),
            patch.object(extrakto_plugin.subprocess, "Popen", side_effect=self.fzf),
            patch.dict(os.environ, {"HOME": "/home/extrakto-test"}),
        ]
        for mocked in self.patches:
            mocked.start()
            self.addCleanup(mocked.stop)

    def output(self, argv, **kwargs):
        self.calls.append((argv, kwargs))
        if argv[1] == "show-option":
            return self.options.get(argv[-1], "").encode()
        if argv[1] == "capture-pane":
            return "TASK-12345 /home/test/project.log https://example.test/path?q=1"
        raise AssertionError(f"unexpected command: {argv!r}")

    def fake_run(self, argv, **kwargs):
        self.calls.append((argv, kwargs))
        return subprocess.CompletedProcess(argv, 0)

    def fzf(self, argv, **kwargs):
        self.calls.append((argv, kwargs))
        return FakeFzf(self.fzf_response)

    def test_cli_extracts_path_url_and_id(self):
        text = "TASK-12345 /home/test/project.log https://example.test/path?q=1"
        # Use the saved real runner so this exercises the actual extraction CLI.
        with patch.object(subprocess, "Popen", REAL_POPEN):
            result = REAL_RUN(
                [sys.executable, "-B", str(PLUGIN / "extrakto.py"), "-w", "-p", "-u"],
                input=text, text=True, capture_output=True, check=True,
                env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"},
            )
        self.assertEqual(
            result.stdout.splitlines(),
            ["TASK-12345", "/home/test/project.log",
             "https://example.test/path?q", "https://example.test/path?q=1"],
        )

    def test_full_captures_entire_trigger_pane_history_only(self):
        plugin = extrakto_plugin.ExtraktoPlugin("%12", "popup")
        plugin.capture_panes()
        self.assertEqual(
            self.calls[-1][0],
            ["tmux", "capture-pane", "-pJ", "-S", "-", "-t", "%12"],
        )
        self.assertFalse(any("list-panes" in argv for argv, _ in self.calls))

    def test_copy_uses_native_tmux_osc52_with_literal_text(self):
        plugin = extrakto_plugin.ExtraktoPlugin("%12", "popup")
        plugin.copy("some text;$(id)")
        self.assertEqual(
            self.calls[-1][0], ["tmux", "set-buffer", "-w", "--", "some text;$(id)"]
        )

    def test_fzf_enter_copies_selected_id(self):
        plugin = extrakto_plugin.ExtraktoPlugin("%12", "popup")
        self.assertEqual(plugin.capture(), 0)
        self.assertEqual(
            self.calls[-1][0], ["tmux", "set-buffer", "-w", "--", "TASK-12345"]
        )

    def test_fzf_tab_bracketed_pastes_into_trigger_pane_without_enter(self):
        self.fzf_response = "\ntab\nTASK-12345\n"
        plugin = extrakto_plugin.ExtraktoPlugin("%12", "popup")
        self.assertEqual(plugin.capture(), 0)
        self.assertEqual(self.calls[-2][0], ["tmux", "set-buffer", "--", "TASK-12345"])
        self.assertEqual(
            self.calls[-1][0], ["tmux", "paste-buffer", "-p", "-t", "%12"]
        )
        self.assertFalse(any("send-keys" in argv for argv, _ in self.calls))

    def test_fzf_edit_delegates_to_safe_popup_helper(self):
        self.fzf_response = "\nctrl-e\na file;$(id).txt\n"
        plugin = extrakto_plugin.ExtraktoPlugin("%12", "popup")
        self.assertEqual(plugin.capture(), 0)
        self.assertEqual(
            self.calls[-1][0],
            [
                sys.executable, str(ROOT / "config/workflow/bin/tmux-open-selection.py"),
                "%12", "editor",
            ],
        )
        self.assertEqual(self.calls[-1][1]["input"], "a file;$(id).txt")
        self.assertFalse(any("send-keys" in argv for argv, _ in self.calls))

    def test_open_delegates_to_safe_stdin_helper(self):
        plugin = extrakto_plugin.ExtraktoPlugin("%12", "popup")
        plugin.open("https://example.test/?q=$(id)&x=1")
        self.assertEqual(
            self.calls[-1][0],
            [
                sys.executable, str(ROOT / "config/workflow/bin/tmux-open-selection.py"),
                "%12", "open",
            ],
        )
        self.assertEqual(self.calls[-1][1]["input"], "https://example.test/?q=$(id)&x=1")
        self.assertNotIn("shell", self.calls[-1][1])


REAL_RUN = subprocess.run
REAL_POPEN = subprocess.Popen

if __name__ == "__main__":
    unittest.main()
