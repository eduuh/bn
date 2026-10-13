#!/usr/bin/env python3
"""Durable foreground command results and their terminal/tmux views."""
import argparse
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import uuid


RUN_ID = re.compile(r"[0-9]{20}-[0-9a-f]{32}")
STATES = {"running", "passed", "failed", "cancelled"}
DISPLAY_STATES = {
    "running": "RUN", "passed": "PASS", "failed": "FAIL",
    "cancelled": "CANCEL", "unknown": "UNKNOWN",
}


def workspace_path(cwd):
    cwd = Path(cwd).resolve()
    result = subprocess.run(
        ["git", "-C", str(cwd), "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, timeout=5,
    )
    return Path(result.stdout.rstrip("\n")).resolve() if result.returncode == 0 else cwd


def records_directory(workspace):
    cache = os.environ.get("XDG_CACHE_HOME") or str(Path.home() / ".cache")
    key = hashlib.sha256(str(workspace).encode()).hexdigest()
    return Path(cache) / "bn/commands/runs" / key


class RunRecord:
    def __init__(self, args, log_path):
        cwd = Path.cwd().resolve()
        workspace = workspace_path(cwd)
        directory = records_directory(workspace)
        directory.mkdir(parents=True, exist_ok=True, mode=0o700)
        directory.chmod(0o700)
        directory.parent.chmod(0o700)
        identifier = f"{time.time_ns():020d}-{uuid.uuid4().hex}"
        self.path = directory / f"{identifier}.json"
        fd = os.open(self.path.with_suffix(".lock"), os.O_RDWR | os.O_CREAT | os.O_EXCL, 0o600)
        self.lock = os.fdopen(fd, "r+b")
        self.data = {
            "id": identifier, "workspace": str(workspace), "cwd": str(cwd),
            "command": args.command, "label": args.label or Path(args.command[0]).name,
            "interval": args.interval, "idle_after": args.idle_after,
            "started_at": time.time(), "finished_at": None, "duration": 0,
            "state": "running", "exit_code": None, "log": str(log_path),
        }
        try:
            fcntl.flock(self.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            self.write()
        except OSError:
            self.close()
            raise

    def write(self):
        fd, name = tempfile.mkstemp(prefix=".record-", dir=self.path.parent)
        temporary = Path(name)
        try:
            with os.fdopen(fd, "w") as file:
                json.dump(self.data, file)
                file.write("\n")
            temporary.replace(self.path)
        finally:
            temporary.unlink(missing_ok=True)

    def update(self, duration, code=None, cancelled=False):
        self.data["duration"] = duration
        if code is not None:
            self.data.update(
                exit_code=code, finished_at=time.time(),
                state="cancelled" if cancelled else "passed" if code == 0 else "failed",
            )
        self.write()

    def close(self):
        self.lock.close()


def validate_record(record, path, workspace):
    if not isinstance(record, dict):
        raise ValueError(f"invalid run record: {path}")
    strings = ("id", "workspace", "cwd", "label", "log")
    numbers = ("started_at", "duration", "interval", "idle_after")
    command = record.get("command")
    valid = (
        all(isinstance(record.get(key), str) and "\0" not in record[key] for key in strings)
        and all(type(record.get(key)) in (int, float) and math.isfinite(record[key])
                for key in numbers)
        and record["id"] == path.stem and RUN_ID.fullmatch(record["id"])
        and record["workspace"] == str(workspace)
        and Path(record["cwd"]).is_absolute() and Path(record["log"]).is_absolute()
        and isinstance(record.get("state"), str) and record["state"] in STATES
        and isinstance(command, list) and command
        and all(isinstance(arg, str) and "\0" not in arg for arg in command)
        and record["duration"] >= 0 and record["interval"] > 0 and record["idle_after"] > 0
    )
    if valid:
        if record["state"] == "running":
            valid = record.get("exit_code") is None and record.get("finished_at") is None
        else:
            valid = (
                type(record.get("exit_code")) is int
                and record["exit_code"] >= 0
                and type(record.get("finished_at")) in (int, float)
                and math.isfinite(record["finished_at"])
                and (record["state"] == "cancelled"
                     or (record["exit_code"] == 0) == (record["state"] == "passed"))
            )
    if not valid:
        raise ValueError(f"invalid run record: {path}")


def runner_is_alive(path):
    try:
        with path.with_suffix(".lock").open("r+b") as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                return True
        return False
    except FileNotFoundError:
        return False


def read_records(cwd):
    workspace = workspace_path(cwd)
    records = []
    for path in sorted(records_directory(workspace).glob("*.json"), reverse=True):
        try:
            record = json.loads(path.read_text())
        except json.JSONDecodeError as error:
            raise ValueError(f"invalid run record: {path}: {error}") from error
        validate_record(record, path, workspace)
        if record["state"] == "running":
            if runner_is_alive(path):
                record["duration"] = max(0, time.time() - record["started_at"])
            else:
                # Re-read after taking the lock: the writer may have just committed its result.
                record = json.loads(path.read_text())
                validate_record(record, path, workspace)
                if record["state"] == "running":
                    record["state"] = "unknown"
        records.append(record)
    return records


def display_text(text):
    return "".join(character for character in text if character.isprintable())


def summary_parts(records):
    latest = {}
    for record in records:
        latest.setdefault(record["label"], record)
    running = [record for record in records if record["state"] == "running"]
    failed = [record for record in latest.values() if record["state"] == "failed"]
    unknown = [record for record in latest.values() if record["state"] == "unknown"]
    chosen = (running or failed or unknown or records)[0]
    return chosen, running, failed, unknown


def summary(records, badge=False):
    if not records:
        return ""
    chosen, running, failed, unknown = summary_parts(records)
    state = DISPLAY_STATES[chosen["state"]]
    text = state if badge else f"{display_text(chosen['label'])[:24]} {state}"
    if chosen["state"] == "running":
        text += str(len(running)) if badge and len(running) > 1 else ""
        text += f" {int(chosen['duration'])}s" if not badge else ""
        if len(running) > 1 and not badge:
            text += f" +{len(running) - 1}"
    elif chosen["state"] == "failed" and not badge:
        text += f" {chosen['exit_code']}"
    if failed and chosen["state"] != "failed":
        text += f" !{len(failed)}"
    if unknown and chosen["state"] != "unknown":
        text += f" ?{len(unknown)}"
    return text


def tmux_summary(records, badge=False):
    text = summary(records, badge)
    if not text:
        return ""
    chosen, running, failed, unknown = summary_parts(records)
    color = "#f7768e" if failed else (
        "#e0af68" if unknown or chosen["state"] == "cancelled" else (
            "#7dcfff" if running else "#9ece6a"
        )
    )
    return f"#[fg={color}] {text.replace('#', '##')} #[default]"


def find_record(cwd, identifier):
    if not RUN_ID.fullmatch(identifier):
        raise ValueError("invalid run id")
    for record in read_records(cwd):
        if record["id"] == identifier:
            return record
    raise ValueError(f"run id not found in this worktree: {identifier}")


def rerun(record):
    if record["state"] == "running":
        raise ValueError("command is still running")
    if workspace_path(record["cwd"]) != Path(record["workspace"]):
        raise ValueError("original working directory no longer belongs to this workspace")
    return subprocess.run(
        [sys.executable, str(Path(__file__).with_name("run.py")),
         "--label", record["label"], "--interval", str(record["interval"]),
         "--idle-after", str(record["idle_after"]), "--", *record["command"]],
        cwd=record["cwd"],
    ).returncode


def show_logs(record):
    with Path(record["log"]).open("rb") as log:
        shutil.copyfileobj(log, sys.stdout.buffer)
    return 0


def pick(cwd):
    if not shutil.which("fzf"):
        raise ValueError("fzf is not installed; use --json, --logs or --rerun")
    while True:
        records = read_records(cwd)
        if not records:
            print("No command runs in this worktree. Press Enter to close.")
            input()
            return 0
        rows = "\n".join(
            f"{record['id']}\t{display_text(record['label'])[:40]} "
            f"{DISPLAY_STATES[record['state']]} {record['duration']:.1f}s"
            for record in records
        )
        selected = subprocess.run(
            ["fzf", "--reverse", "--delimiter", "\t", "--with-nth", "2",
             "--header", "Command runs: Enter inspects, Esc closes"],
            input=rows, capture_output=True, text=True,
        )
        if selected.returncode in (1, 130):
            return 0
        if selected.returncode:
            raise ValueError(f"fzf exited with status {selected.returncode}: {selected.stderr.strip()}")
        record = find_record(cwd, selected.stdout.split("\t")[0])
        print(f"\n{display_text(record['label'])}: {DISPLAY_STATES[record['state']]}")
        print(f"cwd: {display_text(record['cwd'])}")
        print(f"command: {display_text(shlex.join(record['command']))}")
        print(f"log: {display_text(record['log'])}")
        print("l: logs | r: rerun with current environment | Enter: back | q: close")
        action = input("> ").strip().lower()
        if action == "q":
            return 0
        if action == "l":
            if shutil.which("less"):
                subprocess.run(["less", "--", record["log"]], check=True)
            else:
                show_logs(record)
                input("\nPress Enter to return.")
        elif action == "r":
            if input("Run this command again? [y/N] ").strip().lower() == "y":
                code = rerun(find_record(cwd, record["id"]))
                input(f"\nExited {code}. Press Enter to return.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--json", action="store_true", help="list this workspace's runs as JSON")
    modes.add_argument("--status", action="store_true", help="compact summary")
    modes.add_argument("--badge", action="store_true", help="short window-tab summary")
    modes.add_argument("--logs", metavar="ID", help="print a run's retained output")
    modes.add_argument("--rerun", metavar="ID", help="rerun original argv/cwd in the current environment")
    modes.add_argument("--pick", action="store_true", help="interactive logs/rerun picker")
    parser.add_argument("--tmux", action="store_true", help="escape and color status for tmux")
    parser.add_argument("--path", default=".", help="workspace or subdirectory (default: cwd)")
    args = parser.parse_args()
    try:
        if args.logs:
            return show_logs(find_record(args.path, args.logs))
        if args.rerun:
            return rerun(find_record(args.path, args.rerun))
        if args.pick:
            return pick(args.path)
        records = read_records(args.path)
        if args.status or args.badge:
            render = tmux_summary if args.tmux else summary
            print(render(records, args.badge), end="")
        elif args.json:
            print(json.dumps(records))
        else:
            for record in records:
                print(f"{record['id']}  {display_text(record['label'])}  "
                      f"{DISPLAY_STATES[record['state']]}  {record['duration']:.1f}s")
        return 0
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"runs: {error}", file=sys.stderr)
        if args.status or args.badge:
            print("RUNS ERROR", end="")
        if args.pick and sys.stdin.isatty():
            try:
                input("Press Enter to close.")
            except (EOFError, KeyboardInterrupt):
                pass
        return 1
    except (EOFError, KeyboardInterrupt):
        return 130


if __name__ == "__main__":
    sys.exit(main())
