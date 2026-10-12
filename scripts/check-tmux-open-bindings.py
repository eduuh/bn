#!/usr/bin/env python3
"""Load plugin bindings on a private tmux socket; never touch the developer server."""

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import time
import unittest
from unittest.mock import patch
import uuid

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
OPEN = ROOT / "config/workflow/plugins/tmux-open/open.tmux"
EXTRAKTO = ROOT / "config/workflow/plugins/extrakto"


class BindingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmux = shutil.which("tmux")
        if not cls.tmux:
            raise RuntimeError("tmux is required for isolated binding checks")
        cls.scratch = ROOT / "scripts" / (".check-tmux-open-" + uuid.uuid4().hex[:8])
        cls.scratch.mkdir()
        cls.addClassCleanup(shutil.rmtree, cls.scratch)
        cls.socket = cls.scratch / "socket"
        cls.bin = cls.scratch / "bin"
        cls.bin.mkdir()
        cls.home = cls.scratch / "home"
        (cls.home / ".local/bin").mkdir(parents=True)
        (cls.home / ".config/bn").mkdir(parents=True)
        cls.log = cls.scratch / "opened.json"
        cls.directory = cls.scratch / "a project's cwd"
        cls.directory.mkdir()
        wrapper = cls.bin / "tmux"
        wrapper.write_text(
            "#!/bin/sh\nexec " + shlex.quote(cls.tmux) + ' -S "$CHECK_SOCKET" "$@"\n'
        )
        wrapper.chmod(0o755)
        bn = cls.home / ".config/bn/bn"
        bn.write_text(
            "#!" + sys.executable + "\n"
            "import json, os, sys\n"
            "from pathlib import Path\n"
            "Path(os.environ['CHECK_LOG']).write_text("
            "json.dumps({'argv': sys.argv[1:], 'cwd': os.getcwd()}))\n"
        )
        bn.chmod(0o755)
        fzf = cls.home / ".local/bin/fzf"
        fzf.write_text("#!/bin/sh\nexit 0\n")
        fzf.chmod(0o755)
        cls.env = {
            **os.environ,
            "HOME": str(cls.home),
            "PATH": str(cls.bin) + ":/usr/bin:/bin",
            "CHECK_SOCKET": str(cls.socket),
            "CHECK_LOG": str(cls.log),
            "PYTHONDONTWRITEBYTECODE": "1",
        }
        cls.env.pop("TMUX", None)
        cls.run_tmux(
            "-f", "/dev/null", "new-session", "-d", "-s", "plugin-check",
            "-c", str(cls.directory), "sleep 120",
        )
        cls.addClassCleanup(cls.run_tmux, "kill-server")
        cls.pane = cls.run_tmux("display-message", "-p", "#{pane_id}").stdout.strip()
        cls.run_tmux("set-option", "-g", "@open", "M-o")
        cls.run_tmux("set-option", "-g", "@open-editor", "M-e")
        cls.run_tmux("set-option", "-g", "@open-M-s", "https://www.google.com/search?q=")
        cls.run_tmux("set-option", "-g", "@extrakto_key", "none")
        cls.run_tmux("bind-key", "-T", "copy-mode-vi", "y",
                     "send-keys", "-X", "copy-selection-and-cancel")
        cls.original_o = cls.binding_line("copy-mode-vi", "o")
        cls.original_y = cls.binding_line("copy-mode-vi", "y")
        subprocess.run(["bash", str(OPEN)], check=True, env=cls.env, capture_output=True)

    @classmethod
    def run_tmux(cls, *args):
        return subprocess.run(
            [cls.tmux, "-S", str(cls.socket), *args],
            env=cls.env, check=True, capture_output=True, text=True,
        )

    def binding_command(self, table, key):
        line = self.binding_line(table, key)
        self.assertIn("copy-pipe-and-cancel", line)
        return shlex.split(line)[-1]

    @classmethod
    def binding_line(cls, table, key):
        lines = cls.run_tmux("list-keys", "-T", table).stdout.splitlines()
        return next((line for line in lines if shlex.split(line)[3] == key), "")

    def run_binding_pipe(self, command, text):
        subprocess.run(
            command.replace("#{pane_id}", self.pane),
            shell=True, input=text, text=True, env=self.env, check=True,
            capture_output=True,
        )
        return json.loads(self.log.read_text())

    def test_open_binding_loads_for_both_copy_tables(self):
        vi = self.binding_command("copy-mode-vi", "M-o")
        emacs = self.binding_command("copy-mode", "M-o")
        self.assertEqual(vi, emacs)
        self.assertEqual(shlex.split(vi)[-2:], ["#{pane_id}", "open"])

    def test_editor_binding_is_popup_helper_not_send_keys_command(self):
        command = self.binding_command("copy-mode-vi", "M-e")
        self.assertEqual(shlex.split(command)[-2:], ["#{pane_id}", "editor"])
        self.assertNotIn("send-keys", command)

    def test_hyphenated_search_key_loads_without_becoming_M(self):
        command = self.binding_command("copy-mode-vi", "M-s")
        self.assertEqual(
            shlex.split(command)[-3:],
            ["#{pane_id}", "search", "https://www.google.com/search?q="],
        )

    def test_stock_other_end_and_existing_y_are_preserved(self):
        self.assertIn("other-end", self.original_o)
        self.assertEqual(
            self.binding_line("copy-mode-vi", "o"),
            self.original_o,
        )
        self.assertEqual(
            self.binding_line("copy-mode-vi", "y"),
            self.original_y,
        )

    def test_generated_pipe_passes_malicious_looking_path_to_installed_bn_as_data(self):
        command = self.binding_command("copy-mode-vi", "M-o")
        opened = self.run_binding_pipe(command, "a 'quote';$(touch owned).txt")
        self.assertEqual(
            opened["argv"],
            ["open", "--", str(self.directory) + "/a 'quote';$(touch owned).txt"],
        )
        self.assertEqual(opened["cwd"], str(self.directory))
        self.assertFalse((self.directory / "owned").exists())

    def test_generated_search_pipe_urlencodes_plain_multiword_text(self):
        command = self.binding_command("copy-mode-vi", "M-s")
        opened = self.run_binding_pipe(command, "one two & $(id)")
        self.assertEqual(
            opened["argv"],
            ["open", "--", "https://www.google.com/search?q=one+two+%26+%24%28id%29"],
        )

    def test_engine_quotes_are_data_in_generated_shell_command(self):
        self.run_tmux(
            "set-option", "-g", "@open-M-s", "https://example.test/?literal='quoted'&q="
        )
        try:
            subprocess.run(["bash", str(OPEN)], check=True, env=self.env, capture_output=True)
            command = self.binding_command("copy-mode-vi", "M-s")
            opened = self.run_binding_pipe(command, "hello there")
            self.assertEqual(
                opened["argv"],
                ["open", "--", "https://example.test/?literal='quoted'&q=hello+there"],
            )
        finally:
            self.run_tmux(
                "set-option", "-g", "@open-M-s", "https://www.google.com/search?q="
            )
            subprocess.run(["bash", str(OPEN)], check=True, env=self.env, capture_output=True)

    def test_extrakto_none_does_not_overwrite_prefix_tab(self):
        before = self.binding_line("prefix", "Tab")
        subprocess.run(
            ["sh", str(EXTRAKTO / "extrakto.tmux")],
            env=self.env, check=True, capture_output=True,
        )
        self.assertEqual(
            self.binding_line("prefix", "Tab"), before
        )

    def test_extrakto_popup_startup_quotes_paths_and_supplies_python_fzf_path(self):
        fake_bin = self.scratch / "fake-bin"
        fake_bin.mkdir()
        fake_tmux = fake_bin / "tmux"
        startup_log = self.scratch / "startup.json"
        fake_tmux.write_text(
            "#!" + sys.executable + "\n"
            "import json, os, sys\n"
            "from pathlib import Path\n"
            "args = sys.argv[1:]\n"
            "if args[0] == 'show-option':\n"
            "    pass\n"
            "elif args[0] == 'list-commands':\n"
            "    print('popup')\n"
            "elif args[0] == 'popup':\n"
            "    Path(os.environ['CHECK_STARTUP_LOG']).write_text(json.dumps(args))\n"
            "else:\n"
            "    raise SystemExit('unexpected fake tmux command: ' + repr(args))\n"
        )
        fake_tmux.chmod(0o755)
        alias = self.scratch / "plugin a 'quote'"
        alias.symlink_to(EXTRAKTO, target_is_directory=True)
        env = {
            **self.env, "PATH": str(fake_bin) + ":/usr/bin:/bin",
            "CHECK_STARTUP_LOG": str(startup_log),
        }
        subprocess.run(
            ["sh", str(alias / "scripts/open.sh"), "%12"],
            env=env, check=True, capture_output=True,
        )
        args = json.loads(startup_log.read_text())
        command = shlex.split(args[-1])
        self.assertEqual(command[0], "env")
        self.assertTrue(command[1].startswith("PATH=" + str(fake_bin)))
        self.assertIn(str(self.home / ".local/bin"), command[1])
        self.assertTrue(os.path.isabs(command[2]))
        self.assertEqual(Path(command[2]).name, "python3")
        self.assertEqual(command[-3:], [str(alias / "scripts/../extrakto_plugin.py"), "%12", "popup"])
        self.assertNotIn("send-keys", args)

    def test_real_extrakto_capture_fzf_copy_and_insert(self):
        real_fzf = shutil.which("fzf")
        if not real_fzf:
            self.fail("fzf is required for the real capture/copy/insert test")
        receiver = self.scratch / "receiver.py"
        inserted = self.scratch / "inserted.txt"
        receiver.write_text(
            "import os, sys, tty\nfrom pathlib import Path\n"
            "tty.setraw(sys.stdin.fileno())\n"
            "os.write(1, b'TASK-12345\\r\\n')\n"
            "while True:\n"
            "    data = os.read(0, 4096)\n"
            "    with Path(sys.argv[1]).open('ab') as output:\n"
            "        output.write(data)\n"
        )
        command = shlex.join([sys.executable, str(receiver), str(inserted)])
        pane = self.run_tmux(
            "new-window", "-d", "-P", "-F", "#{pane_id}",
            "-c", str(self.directory), command,
        ).stdout.strip()
        choice = self.scratch / "fzf-choice.py"
        choice.write_text(
            "#!" + sys.executable + "\n"
            "import os, subprocess, sys\n"
            "result = subprocess.run("
            "[os.environ['CHECK_REAL_FZF'], '--filter=TASK-12345', '--no-sort'], "
            "input=sys.stdin.read(), text=True, capture_output=True, check=True)\n"
            "print('\\n' + os.environ['CHECK_FZF_KEY'] + '\\n' + result.stdout, end='')\n"
        )
        choice.chmod(0o755)
        self.run_tmux("set-option", "-g", "@extrakto_fzf_tool", str(choice))
        self.run_tmux("set-option", "-g", "@extrakto_clip_tool", "unused")
        self.run_tmux("set-option", "-g", "@extrakto_clip_mode", "tmux_osc52")
        self.run_tmux("set-option", "-g", "@extrakto_grab_area", "full")
        self.run_tmux("set-option", "-g", "@extrakto_filter_order", "word path url line")
        sys.path.insert(0, str(EXTRAKTO))
        import extrakto_plugin
        env = {**self.env, "CHECK_REAL_FZF": real_fzf, "CHECK_FZF_KEY": "enter"}
        try:
            with patch.dict(os.environ, env, clear=True):
                plugin = extrakto_plugin.ExtraktoPlugin(pane, "popup")
                self.assertIn("TASK-12345", plugin.capture_panes())
                self.assertEqual(plugin.capture(), 0)
            self.assertEqual(self.run_tmux("show-buffer").stdout, "TASK-12345")
            env["CHECK_FZF_KEY"] = "tab"
            with patch.dict(os.environ, env, clear=True):
                self.assertEqual(extrakto_plugin.ExtraktoPlugin(pane, "popup").capture(), 0)
            deadline = time.monotonic() + 2
            while not inserted.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertEqual(inserted.read_bytes(), b"TASK-12345")
        finally:
            self.run_tmux("kill-pane", "-t", pane)


if __name__ == "__main__":
    unittest.main()
