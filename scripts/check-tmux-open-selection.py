#!/usr/bin/env python3
"""Selection actions with fake tmux/bn at subprocess boundaries."""

import importlib.util
import io
import os
from pathlib import Path
import shlex
import subprocess
import sys
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "tmux_open_selection", ROOT / "config/workflow/bin/tmux-open-selection.py"
)
selection_helper = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(selection_helper)


class SelectionTests(unittest.TestCase):
    def setUp(self):
        self.calls = []
        self.cwd = "/work/a project's dir"
        self.stdout = ""
        self.failure = None
        self.run_patch = patch.object(
            selection_helper.subprocess, "run", side_effect=self.fake_run
        )
        self.run_patch.start()
        self.addCleanup(self.run_patch.stop)
        self.home_patch = patch.dict(os.environ, {"HOME": "/home/tester"})
        self.home_patch.start()
        self.addCleanup(self.home_patch.stop)
        self.which_patch = patch.object(
            selection_helper.shutil, "which", return_value="/tools/nvim"
        )
        self.which_patch.start()
        self.addCleanup(self.which_patch.stop)

    def fake_run(self, argv, **kwargs):
        self.calls.append((argv, kwargs))
        if argv[:2] == ["tmux", "display-message"] and "-p" in argv:
            return subprocess.CompletedProcess(argv, 0, self.cwd + "\n", "")
        if argv[0] == "/home/tester/.config/bn/bn":
            if self.failure:
                raise self.failure
            return subprocess.CompletedProcess(argv, 0, self.stdout, "")
        return subprocess.CompletedProcess(argv, 0, "", "")

    def test_relative_path_with_spaces_and_quotes_stays_one_argument(self):
        selection_helper.execute("%12", "open", "a 'quoted' file.txt")
        self.assertEqual(
            self.calls[-1][0],
            [
                "/home/tester/.config/bn/bn", "open", "--",
                "/work/a project's dir/a 'quoted' file.txt",
            ],
        )
        self.assertEqual(self.calls[-1][1]["cwd"], "/work/a project's dir")
        self.assertNotIn("shell", self.calls[-1][1])

    def test_shell_metacharacters_stay_literal_path_data(self):
        selection_helper.execute("%12", "open", "$(touch owned); `id` & file.txt")
        self.assertEqual(
            self.calls[-1][0],
            [
                "/home/tester/.config/bn/bn", "open", "--",
                "/work/a project's dir/$(touch owned); `id` & file.txt",
            ],
        )

    def test_url_fragment_and_metacharacters_are_not_shell_evaluated(self):
        selection_helper.execute("%12", "open", "https://example.test/?q=$(id)&x=1#anchor")
        self.assertEqual(
            self.calls[-1][0],
            [
                "/home/tester/.config/bn/bn", "open", "--",
                "https://example.test/?q=$(id)&x=1#anchor",
            ],
        )

    def test_search_encodes_multiword_shell_text_and_unicode(self):
        selection_helper.execute(
            "%12", "search", "one two & $(id) café", "https://www.google.com/search?q="
        )
        self.assertEqual(
            self.calls[-1][0],
            [
                "/home/tester/.config/bn/bn", "open", "--",
                "https://www.google.com/search?q=one+two+%26+%24%28id%29+caf%C3%A9",
            ],
        )

    def test_search_encodes_newlines_instead_of_concatenating_them(self):
        selection_helper.execute(
            "%12", "search", "one\n two", "https://example.test/?q="
        )
        self.assertEqual(
            self.calls[-1][0][-1], "https://example.test/?q=one%0A+two"
        )

    def test_editor_popup_targets_pane_and_cwd_without_typing_into_it(self):
        selection_helper.execute("%12", "editor", "a 'quote';$(id).txt")
        self.assertEqual(
            self.calls[-1][0][:11],
            [
                "tmux", "display-popup", "-E", "-t", "%12", "-d",
                "/work/a project's dir", "-w", "90%", "-h", "90%",
            ],
        )
        self.assertEqual(
            shlex.split(self.calls[-1][0][-1]),
            ["exec", "/tools/nvim", "--", "/work/a project's dir/a 'quote';$(id).txt"],
        )
        self.assertFalse(any("send-keys" in argv for argv, _ in self.calls))

    def test_editor_rejects_multiline_paths(self):
        with self.assertRaisesRegex(ValueError, "single line"):
            selection_helper.execute("%12", "editor", "one\ntwo")
        self.assertEqual(self.calls, [])

    def test_open_rejects_nul(self):
        with self.assertRaisesRegex(ValueError, "control"):
            selection_helper.execute("%12", "open", "one\0two")
        self.assertEqual(self.calls, [])

    def test_empty_selection_is_an_error(self):
        with self.assertRaisesRegex(ValueError, "empty"):
            selection_helper.execute("%12", "open", " ")

    def test_invalid_pane_cannot_be_used_as_tmux_target(self):
        with self.assertRaisesRegex(ValueError, "pane"):
            selection_helper.execute("%12; touch owned", "open", "test.txt")
        self.assertEqual(self.calls, [])

    def test_search_rejects_non_http_engine(self):
        with self.assertRaisesRegex(ValueError, "HTTP"):
            selection_helper.execute("%12", "search", "test", "file:///")
        self.assertEqual(self.calls, [])

    def test_editor_rejects_urls(self):
        with self.assertRaisesRegex(ValueError, "file"):
            selection_helper.execute("%12", "editor", "https://example.test/")

    def test_missing_nvim_is_explicit(self):
        selection_helper.shutil.which.return_value = None
        with self.assertRaisesRegex(ValueError, "nvim"):
            selection_helper.execute("%12", "editor", "test.txt")

    def test_bn_print_only_fallback_is_an_error(self):
        self.stdout = "https://example.test/\n"
        with self.assertRaisesRegex(ValueError, "opener"):
            selection_helper.execute("%12", "open", "https://example.test/")

    def test_failure_surfaces_literal_tmux_message_stderr_and_status(self):
        self.failure = subprocess.CalledProcessError(
            7, ["bn", "open"], stderr="opener failed"
        )
        with patch.object(sys, "stdin", io.StringIO("test.txt")), patch.object(
            sys, "stderr", io.StringIO()
        ) as stderr:
            status = selection_helper.main(["%12", "open"])
        self.assertEqual(status, 1)
        self.assertIn("opener failed", stderr.getvalue())
        self.assertEqual(
            self.calls[-1][0][:6],
            ["tmux", "display-message", "-l", "-t", "%12", "--"],
        )
        self.assertIn("opener failed", self.calls[-1][0][-1])

    def test_missing_installed_bn_is_not_silently_replaced(self):
        self.failure = FileNotFoundError("installed bn not found")
        with patch.object(sys, "stdin", io.StringIO("test.txt")), patch.object(
            sys, "stderr", io.StringIO()
        ) as stderr:
            status = selection_helper.main(["%12", "open"])
        self.assertEqual(status, 1)
        self.assertIn("installed bn not found", stderr.getvalue())
        self.assertEqual(
            self.calls[-2][0][0], "/home/tester/.config/bn/bn"
        )


if __name__ == "__main__":
    unittest.main()
