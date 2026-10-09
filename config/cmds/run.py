#!/usr/bin/env python3
"""Stream a foreground job without leaving a quiet command looking stuck."""
import argparse
import math
import os
from pathlib import Path
import selectors
import signal
import subprocess
import sys
import tempfile
import time


def positive_seconds(value):
    try:
        number = float(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("must be a positive finite number") from error
    if not math.isfinite(number) or number <= 0:
        raise argparse.ArgumentTypeError("must be a positive finite number")
    return number


def open_log(path):
    if path:
        path = Path(path).resolve()
        path.parent.mkdir(parents=True, exist_ok=True)
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    else:
        cache = os.environ.get("XDG_CACHE_HOME") or str(Path.home() / ".cache")
        directory = Path(cache) / "bn" / "commands"
        directory.mkdir(parents=True, exist_ok=True)
        fd, path = tempfile.mkstemp(prefix="run-", suffix=".log", dir=directory)
        path = Path(path).resolve()
    return os.fdopen(fd, "wb"), path


def run(args, log, log_path):
    start = time.monotonic()
    last_output = start
    next_report = start + args.interval
    label = args.label or Path(args.command[0]).name
    child = None
    interrupted = None

    def report(action, code=None, idle=None):
        text = f"{action}: {label} | {time.monotonic() - start:.1f}s elapsed | log: {log_path}"
        if code is not None:
            text += f" | exit {code}"
        if idle is not None:
            text += f" | WARNING: no output for {idle:.1f}s; progress unknown"
        print(text, file=sys.stderr, flush=True)

    def forward(signum, _frame):
        nonlocal interrupted
        interrupted = signum
        if child is not None:
            try:
                os.killpg(child.pid, signum)
            except ProcessLookupError:
                pass

    previous = {sig: signal.signal(sig, forward) for sig in (signal.SIGINT, signal.SIGTERM)}
    try:
        report("Starting")
        if interrupted is not None:
            code = 128 + interrupted
            report("Finished", code)
            return code
        try:
            child = subprocess.Popen(
                args.command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                start_new_session=True,
            )
        except OSError as error:
            print(f"run: could not start {label}: {error}", file=sys.stderr, flush=True)
            code = 127 if isinstance(error, FileNotFoundError) else 126
            report("Finished", code)
            return code
        if interrupted is not None:
            forward(interrupted, None)

        with selectors.DefaultSelector() as selector:
            selector.register(child.stdout, selectors.EVENT_READ, sys.stdout.buffer)
            selector.register(child.stderr, selectors.EVENT_READ, sys.stderr.buffer)
            while True:
                code = child.poll()
                wait = min(0.1, max(0, next_report - time.monotonic()))
                events = selector.select(0 if code is not None else wait)
                for key, _mask in events:
                    data = os.read(key.fd, 65536)
                    if not data:
                        selector.unregister(key.fileobj)
                        key.fileobj.close()
                        continue
                    last_output = time.monotonic()
                    log.write(data)
                    log.flush()
                    key.data.write(data)
                    key.data.flush()
                if code is not None and not events:
                    break
                now = time.monotonic()
                if code is None and now >= next_report:
                    idle = now - last_output
                    report("Still running", idle=idle if idle >= args.idle_after else None)
                    next_report = now + args.interval

        code = child.wait()
        code = code if code >= 0 else 128 - code
        report("Finished", code)
        return code
    finally:
        if child is not None:
            if child.poll() is None:
                forward(signal.SIGTERM, None)
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    forward(signal.SIGKILL, None)
                    child.wait()
            child.stdout.close()
            child.stderr.close()
        for sig, handler in previous.items():
            signal.signal(sig, handler)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--label", help="stage name shown in status messages; defaults to the executable")
    parser.add_argument("--interval", type=positive_seconds, default=60, help="heartbeat seconds (default: 60)")
    parser.add_argument("--idle-after", type=positive_seconds, default=180, help="warn after seconds without output (default: 180)")
    parser.add_argument("--log", help="new log file; existing files are never overwritten")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.command[:1] == ["--"]:
        args.command = args.command[1:]
    if not args.command:
        parser.error("a command is required")
    if os.name != "posix":
        parser.error("this command requires a POSIX platform")
    try:
        log, path = open_log(args.log)
        with log:
            return run(args, log, path)
    except OSError as error:
        print(f"run: {error}", file=sys.stderr, flush=True)
        return 1


if __name__ == "__main__":
    sys.exit(main())
