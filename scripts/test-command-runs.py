#!/usr/bin/env python3
"""Exercise durable command results through the shipped runner and inspector."""
import json
import fcntl
import os
from pathlib import Path
import pty
import select
import shlex
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unittest


ROOT = Path(__file__).resolve().parents[1]
RUNNER = ROOT / "config/cmds/run.sh"
INSPECTOR = ROOT / "config/cmds/runs.sh"


class RunTestCase(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="bn-command-runs-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.workspace = self.root / "workspace with spaces"
        self.workspace.mkdir()
        self.env = dict(os.environ, XDG_CACHE_HOME=str(self.root / "cache"))

    def command(self, code, *arguments, label="build", cwd=None):
        return subprocess.run(
            ["/bin/sh", str(RUNNER), "--label", label, "--",
             sys.executable, "-c", code, *arguments],
            cwd=cwd or self.workspace, env=self.env,
            capture_output=True, text=True, timeout=8,
        )

    def inspect(self, *arguments, cwd=None, check=True):
        return subprocess.run(
            ["/bin/sh", str(INSPECTOR), *arguments],
            cwd=cwd or self.workspace, env=self.env,
            capture_output=True, text=True, check=check, timeout=8,
        )

    def records(self, cwd=None):
        return json.loads(self.inspect("--json", cwd=cwd).stdout)

    @staticmethod
    def realpath(path):
        return str(Path(path).resolve())

    def launch(self, code, label="build"):
        process = subprocess.Popen(
            ["/bin/sh", str(RUNNER), "--label", label, "--interval", "0.05",
             "--", sys.executable, "-c", code],
            cwd=self.workspace, env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        self.addCleanup(self.stop, process)
        ready, _, _ = select.select([process.stdout], [], [], 5)
        self.assertTrue(ready, "child did not report readiness")
        self.assertEqual(process.stdout.readline(), b"READY\n")
        return process

    @staticmethod
    def stop(process):
        if process.poll() is None:
            process.terminate()
        process.communicate(timeout=5)


class RunTests(RunTestCase):
    def test_failure_persists_with_exit_status_and_private_log(self):
        result = self.command("print('failure details'); raise SystemExit(37)")
        self.assertEqual(result.returncode, 37, result.stderr)
        records = self.records()
        self.assertEqual(len(records), 1)
        record = records[0]
        self.assertEqual(record["state"], "failed")
        self.assertEqual(record["exit_code"], 37)
        self.assertEqual(record["cwd"], self.realpath(self.workspace))
        self.assertGreaterEqual(record["duration"], 0)
        self.assertIsNotNone(record["finished_at"])
        log = Path(record["log"])
        self.assertEqual(log.read_text(), "failure details\n")
        self.assertEqual(log.stat().st_mode & 0o777, 0o600)
        metadata = list((self.root / "cache/bn/commands/runs").glob("*/*.json"))
        self.assertEqual(len(metadata), 1)
        self.assertEqual(metadata[0].stat().st_mode & 0o777, 0o600)
        self.assertEqual(metadata[0].parent.stat().st_mode & 0o777, 0o700)
        self.assertNotIn("environment", record)

    def test_success_and_missing_executable_have_distinct_results(self):
        self.assertEqual(self.command("pass").returncode, 0)
        self.assertEqual(self.records()[0]["state"], "passed")
        result = subprocess.run(
            ["/bin/sh", str(RUNNER), "--", str(self.root / "missing")],
            cwd=self.workspace, env=self.env, capture_output=True, timeout=5,
        )
        self.assertEqual(result.returncode, 127)
        self.assertEqual(self.records()[0]["state"], "failed")
        self.assertEqual(self.records()[0]["exit_code"], 127)

    def test_quiet_command_stays_running_until_real_exit(self):
        process = self.launch("import time; print('READY', flush=True); time.sleep(100)")
        self.assertEqual(self.records()[0]["state"], "running")
        self.assertIn("build RUN", self.inspect("--status").stdout)
        process.send_signal(signal.SIGTERM)
        process.communicate(timeout=5)
        self.assertEqual(process.returncode, 143)
        self.assertEqual(self.records()[0]["state"], "cancelled")
        self.assertEqual(self.records()[0]["exit_code"], 143)
        self.assertIn("CANCEL", self.inspect("--status").stdout)

    def test_killed_runner_is_unknown_not_success(self):
        pid_file = self.root / "child.pid"
        process = self.launch(
            f"import os,time,pathlib; pathlib.Path({str(pid_file)!r}).write_text(str(os.getpid())); "
            "print('READY', flush=True); time.sleep(100)"
        )
        child_pid = int(pid_file.read_text())
        self.addCleanup(os.kill, child_pid, signal.SIGTERM)
        process.kill()
        process.wait(timeout=5)
        self.assertEqual(self.records()[0]["state"], "unknown")
        self.assertIn("UNKNOWN", self.inspect("--status").stdout)

    def test_concurrent_runs_are_not_overwritten(self):
        process = self.launch("import time; print('READY', flush=True); time.sleep(100)")
        self.assertEqual(self.command("raise SystemExit(2)", label="tests").returncode, 2)
        records = self.records()
        self.assertEqual(len(records), 2)
        self.assertEqual({record["state"] for record in records}, {"running", "failed"})
        self.assertIn("RUN", self.inspect("--status").stdout)
        self.assertIn("!1", self.inspect("--status").stdout)
        process.terminate()
        process.communicate(timeout=5)

    def test_git_worktrees_and_subdirectories_have_correct_scope(self):
        self.git("init", "-q", str(self.workspace))
        self.git("-C", str(self.workspace), "commit", "--allow-empty", "-qm", "seed")
        other = self.root / "other worktree"
        self.git("-C", str(self.workspace), "worktree", "add", "-qb", "other", str(other))
        nested = self.workspace / "nested"
        nested.mkdir()
        self.command("pass", cwd=nested)
        self.command("raise SystemExit(4)", cwd=other)
        self.assertEqual(self.records()[0]["state"], "passed")
        self.assertEqual(self.records(cwd=nested)[0]["cwd"], self.realpath(nested))
        self.assertEqual(self.records(cwd=other)[0]["state"], "failed")
        self.assertNotIn("FAIL", self.inspect("--status").stdout)

    def git(self, *arguments):
        subprocess.run(
            ["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
             "-c", "core.hooksPath=/dev/null", *arguments],
            check=True, capture_output=True, env=self.env, timeout=5,
        )

    def test_rerun_preserves_argv_cwd_and_uses_a_new_log(self):
        arguments = ("two words", "", "$(touch should-not-exist)", "--flag")
        code = "import json,os,sys; print(json.dumps([os.getcwd(), sys.argv[1:]]))"
        original = self.command(code, *arguments)
        self.assertEqual(original.returncode, 0, original.stderr)
        before = self.records()[0]
        result = self.inspect("--rerun", before["id"])
        self.assertEqual(result.stdout, original.stdout)
        after = self.records()[0]
        self.assertNotEqual(before["id"], after["id"])
        self.assertNotEqual(before["log"], after["log"])
        self.assertEqual(after["command"], before["command"])
        self.assertEqual(after["cwd"], before["cwd"])
        self.assertEqual(
            json.loads(result.stdout),
            [self.realpath(self.workspace), list(arguments)],
        )
        self.assertFalse((self.workspace / "should-not-exist").exists())
        self.assertEqual(Path(before["log"]).read_text(), original.stdout)

    def test_log_inspection_returns_original_output(self):
        self.command("print('retained output'); raise SystemExit(9)")
        result = self.inspect("--logs", self.records()[0]["id"])
        self.assertEqual(result.stdout, "retained output\n")

    def test_running_task_cannot_be_rerun(self):
        process = self.launch("import time; print('READY', flush=True); time.sleep(100)")
        result = self.inspect("--rerun", self.records()[0]["id"], check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("still running", result.stderr)
        process.terminate()
        process.communicate(timeout=5)

    def test_failed_result_is_retained_until_same_label_is_superseded(self):
        self.command("raise SystemExit(3)", label="tests")
        self.command("pass", label="build")
        self.assertIn("tests FAIL 3", self.inspect("--status").stdout)
        self.command("pass", label="tests")
        self.assertNotIn("FAIL", self.inspect("--status").stdout)
        self.assertIn("tests PASS", self.inspect("--status").stdout)

    def test_metadata_error_is_visible_and_preserves_command_exit(self):
        directory = self.root / "cache/bn/commands"
        directory.mkdir(parents=True)
        (directory / "runs").write_text("not a directory")
        result = self.command("print('executed'); raise SystemExit(7)")
        self.assertEqual(result.returncode, 7)
        self.assertEqual(result.stdout, "executed\n")
        self.assertIn("could not record", result.stderr)

    def test_bad_record_and_unknown_id_report_errors(self):
        self.command("pass")
        metadata = next((self.root / "cache/bn/commands/runs").glob("*/*.json"))
        metadata.write_text('{"state": "passed"}')
        result = self.inspect("--json", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("record", result.stderr)
        result = self.inspect("--rerun", "../../outside", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("run id", result.stderr)

    def test_empty_workspace_has_no_status_and_lists_no_runs(self):
        self.assertEqual(self.inspect("--status").stdout, "")
        self.assertEqual(self.records(), [])

    def test_command_handling_signal_is_still_cancelled_if_it_returns_zero(self):
        process = self.launch(
            "import signal,time; signal.signal(signal.SIGTERM, lambda *_: exit(0)); "
            "print('READY', flush=True); time.sleep(100)"
        )
        process.terminate()
        process.communicate(timeout=5)
        self.assertEqual(process.returncode, 0)
        self.assertEqual(self.records()[0]["state"], "cancelled")

    def test_tmux_label_is_escaped_and_controls_are_removed(self):
        self.command("pass", label="build#[bg=red]\n\t")
        text = self.inspect("--status", "--tmux").stdout
        self.assertIn("build##[bg=red] PASS", text)
        self.assertNotIn("\n", text)
        self.assertNotIn("\t", text)


class TmuxTests(RunTestCase):
    def setUp(self):
        super().setUp()
        if not shutil.which("tmux"):
            self.fail("tmux is required for isolated command-run status tests")
        self.socket = str(self.root / "socket")
        home = self.root / "home"
        (home / ".config/bn").mkdir(parents=True)
        (home / ".config/bn/repo").symlink_to(ROOT, target_is_directory=True)
        self.env.update(HOME=str(home), TERM="xterm-256color")
        self.env.pop("TMUX", None)
        self.env.pop("TMUX_PANE", None)
        self.tmux("-f", "/dev/null", "new-session", "-d", "-s", "test",
                  "-c", str(self.workspace), "sh")
        self.addCleanup(self.stop_server)
        self.first = self.tmux("display-message", "-p", "#{window_id}").strip()
        self.tmux("set-option", "-g", "status-position", "top")
        self.tmux("set-option", "-g", "status-interval", "1")
        self.tmux("set-option", "-g", "status-left", "#S ")
        config = (ROOT / "config/workflow/tmux.conf").read_text().splitlines()
        lines = [
            line for line in config
            if line.startswith(("setw -g window-status-format ",
                                "setw -g window-status-current-format ",
                                "setw -ga window-status-format ",
                                "setw -ga window-status-current-format ",
                                "set -g status-format[0] "))
            or (line.startswith("bind ") and "build/test runs:" in line)
        ]
        self.assertEqual(len(lines), 6)
        self.tmux("source-file", "-", input="\n".join(lines))
        self.master, self.slave = pty.openpty()
        self.addCleanup(os.close, self.master)
        self.addCleanup(os.close, self.slave)
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 240, 0, 0))
        self.client = subprocess.Popen(
            ["tmux", "-S", self.socket, "attach-session", "-t", "test"],
            env=self.env, stdin=self.slave, stdout=self.slave, stderr=self.slave,
        )
        self.addCleanup(self.stop_client)
        self.screen = b""
        deadline = time.monotonic() + 5
        while not self.tmux("list-clients").strip():
            if time.monotonic() >= deadline:
                self.fail("isolated tmux client did not attach")
            self.drain()

    def tmux(self, *arguments, input=None):
        return subprocess.run(
            ["tmux", "-S", self.socket, *arguments], env=self.env,
            input=input, check=True, capture_output=True, text=True, timeout=5,
        ).stdout

    def stop_server(self):
        subprocess.run(
            ["tmux", "-S", self.socket, "kill-server"],
            env=self.env, capture_output=True, timeout=5,
        )

    def stop_client(self):
        self.client.terminate()
        self.client.wait(timeout=5)

    def drain(self):
        if select.select([self.master], [], [], 0.05)[0]:
            self.screen += os.read(self.master, 65536)

    def wait_for(self, marker):
        deadline = time.monotonic() + 8
        while marker not in self.screen:
            if time.monotonic() >= deadline:
                self.fail(f"tmux did not render {marker!r}: {self.screen[-2000:]!r}")
            self.drain()

    def test_background_window_results_use_their_own_paths(self):
        self.command("raise SystemExit(7)")
        other = self.root / "a project's $(literal) path"
        other.mkdir()
        self.command("pass", cwd=other)
        second = self.tmux("new-window", "-d", "-P", "-F", "#{window_id}",
                           "-c", str(other), "sh").strip()
        self.tmux("refresh-client", "-S")
        self.wait_for(b"FAIL")
        self.wait_for(b"PASS")
        self.assertIn("FAIL", self.inspect("--badge").stdout)
        self.assertEqual(self.inspect("--badge", cwd=other).stdout, "PASS")
        self.tmux("select-window", "-t", second)
        self.screen = b""
        self.tmux("refresh-client", "-S")
        self.wait_for(b"build PASS")
        self.wait_for(b"FAIL")

    def test_command_returning_to_persistent_shell_is_failed(self):
        command = shlex.join([
            "/bin/sh", str(RUNNER), "--label", "pane-build", "--",
            sys.executable, "-c", "raise SystemExit(17)",
        ])
        self.tmux("send-keys", "-t", self.first, "-l", command + "; echo RETURNED_TO_SHELL")
        self.tmux("send-keys", "-t", self.first, "Enter")
        self.wait_for(b"RETURNED_TO_SHELL")
        self.wait_for(b"pane-build FAIL 17")
        self.assertEqual(self.tmux("display-message", "-p", "#{pane_dead}").strip(), "0")

    def test_popup_opens_in_current_worktree_and_closes_on_enter(self):
        self.assertTrue(shutil.which("fzf"), "fzf is required for the runs popup")
        self.screen = b""
        os.write(self.master, b"\x02B")
        self.wait_for(b"No command runs in this worktree.")
        self.screen = b""
        os.write(self.master, b"\r")
        self.wait_for(b"$")
        os.write(self.master, b"printf '%s%s\\n' POPUP_CLOSED_ SUCCESS\r")
        self.wait_for(b"POPUP_CLOSED_SUCCESS")

    def test_real_popup_confirms_and_reruns_selected_failure(self):
        self.command("print('POPUP_OUTPUT'); raise SystemExit(23)")
        original = self.records()[0]
        self.screen = b""
        os.write(self.master, b"\x02B")
        self.wait_for(b"Command runs:")
        os.write(self.master, b"\r")
        self.wait_for(b"r: rerun with current environment")
        os.write(self.master, b"r\r")
        self.wait_for(b"Run this command again?")
        os.write(self.master, b"y\r")
        self.wait_for(b"Exited 23")
        records = self.records()
        self.assertEqual(len(records), 2)
        self.assertEqual(records[0]["state"], "failed")
        self.assertEqual(records[0]["command"], original["command"])
        self.assertNotEqual(records[0]["log"], original["log"])
        self.screen = b""
        os.write(self.master, b"\r")
        self.wait_for(b"Command runs:")
        os.write(self.master, b"\x1b")

    def test_popup_initializes_tool_paths_through_configured_shell(self):
        local_bin = Path(self.env["HOME"]) / ".local/bin"
        local_bin.mkdir(parents=True)
        fzf = shutil.which("fzf")
        self.assertTrue(fzf, "fzf is required for the runs popup")
        (local_bin / "fzf").symlink_to(fzf)
        restricted_bin = self.root / "restricted-bin"
        restricted_bin.mkdir()
        for name in ("sh", "python3", "git", "dirname"):
            (restricted_bin / name).symlink_to(shutil.which(name))
        shell = self.root / "configured-shell"
        shell.write_text(
            '#!/bin/sh\nexport PATH="$HOME/.local/bin:$PATH"\n'
            'printf initialized > "$HOME/shell-started"\nexec /bin/sh "$@"\n'
        )
        shell.chmod(0o700)
        self.tmux("set-environment", "-g", "PATH", str(restricted_bin))
        self.tmux("set-environment", "-g", "SHELL", str(shell))
        binding = next(
            line for line in (ROOT / "config/workflow/tmux.conf").read_text().splitlines()
            if line.startswith("bind ") and "build/test runs:" in line
        )
        self.tmux("source-file", "-", input=binding)
        self.screen = b""
        os.write(self.master, b"\x02B")
        self.wait_for(b"No command runs in this worktree.")
        self.assertEqual(
            (Path(self.env["HOME"]) / "shell-started").read_text(), "initialized",
        )
        os.write(self.master, b"\r")


if __name__ == "__main__":
    unittest.main()
