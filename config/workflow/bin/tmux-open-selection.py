#!/usr/bin/env python3
"""Open copy-mode selections without evaluating them or typing into the source pane."""

import argparse
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
from urllib.parse import quote_plus, urlsplit


def pane_directory(pane):
    result = subprocess.run(
        ["tmux", "display-message", "-p", "-t", pane, "-F", "#{pane_current_path}"],
        check=True, capture_output=True, text=True,
    )
    directory = result.stdout.rstrip("\n")
    if not directory or not os.path.isabs(directory):
        raise ValueError("tmux did not return an absolute pane directory")
    return directory


def is_url(text):
    return bool(re.match(r"^[A-Za-z][A-Za-z0-9+.-]*:", text)) and not bool(
        re.match(r"^[A-Za-z]:[\\/]", text)
    )


def open_target(target, directory):
    bn = str(Path.home() / ".config/bn/bn")
    result = subprocess.run(
        [bn, "open", "--", target],
        cwd=directory, check=True, capture_output=True, text=True,
    )
    # bn prints the target and exits zero when no platform opener is available.
    if result.stdout.strip() == target:
        raise ValueError("bn open found no platform opener")
    if result.stderr:
        sys.stderr.write(result.stderr)


def execute(pane, action, selection, engine=None):
    if not re.fullmatch(r"%[0-9]+", pane):
        raise ValueError("invalid tmux pane id")
    if action not in ("open", "editor", "search"):
        raise ValueError("unknown selection action")
    if any(ord(char) < 32 and char not in "\n\r\t" for char in selection):
        raise ValueError("selection contains control characters")
    if action != "search" and any(char in selection for char in "\n\r"):
        raise ValueError("open/editor selection must be a single line")
    selection = selection.strip()
    if not selection:
        raise ValueError("selection is empty")

    if action == "search":
        parsed = urlsplit(engine or "")
        if parsed.scheme not in ("http", "https") or not parsed.netloc:
            raise ValueError("search engine must be an HTTP(S) URL prefix")
        target = engine + quote_plus(selection)
    else:
        target = selection
        if action == "editor" and is_url(target):
            raise ValueError("editor selection must be a file path, not a URL")

    directory = pane_directory(pane)
    if not is_url(target):
        target = os.path.abspath(os.path.join(directory, os.path.expanduser(target)))
    if action != "editor":
        open_target(target, directory)
        return

    nvim = shutil.which("nvim")
    if not nvim:
        raise ValueError("nvim is not available on PATH")
    command = "exec " + shlex.join([nvim, "--", target])
    subprocess.run(
        ["tmux", "display-popup", "-E", "-t", pane, "-d", directory,
         "-w", "90%", "-h", "90%", command],
        check=True,
    )


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pane")
    parser.add_argument("action", choices=("open", "editor", "search"))
    parser.add_argument("engine", nargs="?")
    args = parser.parse_args(argv)
    os.environ["PATH"] = os.pathsep.join(
        [os.environ.get("PATH", ""), str(Path.home() / ".local/bin"),
         "/usr/local/bin", "/usr/bin", "/bin"]
    )
    try:
        execute(args.pane, args.action, sys.stdin.read(), args.engine)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        detail = error.stderr if isinstance(error, subprocess.CalledProcessError) else None
        message = "tmux-open: " + (detail.strip() if detail else str(error))
        print(message, file=sys.stderr)
        try:
            subprocess.run(
                ["tmux", "display-message", "-l", "-t", args.pane, "--", message],
                check=True,
            )
        except (OSError, subprocess.CalledProcessError) as display_error:
            print(f"tmux-open: unable to display error: {display_error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
