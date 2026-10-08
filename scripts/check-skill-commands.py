#!/usr/bin/env python3
"""Check that every skill renders and every `bn ...` command it cites still exists.

Skills (config/prompts/*.md, .bn/skills/*.md) are instructions an agent follows
literally. When a subcommand or flag is renamed or removed, a skill that still
cites it fails at run time, long after the change that broke it merged. This
walks the built binary's --help tree and checks each `bn` invocation found in
inline code spans and fenced code blocks: every subcommand word must exist, and
every --flag must be accepted by that subcommand (or an ancestor's global flag).

Usage: check-skill-commands.py [--bin target/debug/bn] [files...]
"""
import argparse
import glob
import re
import shlex
import subprocess
import sys

SECTION = re.compile(r"^(Usage|Commands|Options|Arguments):")
CMD_LINE = re.compile(r"^  ([a-z0-9][a-z0-9-]*)(?:\s{2,}(.*))?$")
LONG_FLAG = re.compile(r"(?<![\w-])--([a-z0-9][a-z0-9-]*)")
ALIASES = re.compile(r"\[aliases: ([^\]]+)\]")
SEPARATORS = {"|", "||", "&&", ";", "&", ">", ">>", "2>&1", "2>/dev/null", ")"}


class Node:
    def __init__(self, path):
        self.path = path
        self.children = {}
        self.flags = set()
        self.positional = False
        self.has_commands = False


def load(bin_path, path, cache):
    key = tuple(path)
    if key in cache:
        return cache[key]
    node = Node(path)
    cache[key] = node
    proc = subprocess.run(
        [bin_path, *path, "--help"], capture_output=True, text=True, env={"NO_COLOR": "1", "PATH": "/usr/bin:/bin"}
    )
    out = proc.stdout
    node.exists = proc.returncode == 0
    node.bin, node.cache = bin_path, cache
    section = None
    names = []
    for line in out.splitlines():
        m = SECTION.match(line)
        if m:
            section = m.group(1)
            if section == "Usage":
                node.has_commands |= "COMMAND" in line
            continue
        if section == "Usage" and line.startswith("       bn"):
            node.has_commands |= "COMMAND" in line
        if section == "Commands":
            cm = CMD_LINE.match(line)
            if cm and cm.group(1) != "help":
                am = ALIASES.search(cm.group(2) or "")
                aliases = [a.strip() for a in am.group(1).split(",")] if am else []
                names.append((cm.group(1), aliases))
        elif section == "Arguments":
            if line.startswith("  ") and line.strip():
                node.positional = True
        elif section == "Options":
            if line.startswith("  "):
                spec = line.strip().split("  ")[0]
                node.flags.update(LONG_FLAG.findall(spec))
                am = ALIASES.search(line)
                if am:
                    node.flags.update(a.strip().lstrip("-") for a in am.group(1).split(","))
    for name, aliases in names:
        child = load(bin_path, [*path, name], cache)
        node.children[name] = child
        for alias in aliases:
            node.children.setdefault(alias, child)
    return node


def code_snippets(text):
    """Yield (lineno, snippet) for fenced code lines and inline code spans."""
    fenced = False
    for i, line in enumerate(text.splitlines(), 1):
        if line.lstrip().startswith("```"):
            fenced = not fenced
            continue
        if fenced:
            yield i, line
            continue
        for span in re.findall(r"`([^`]+)`", line):
            yield i, span


# `bn` only where a shell command starts, so "# >>> bn codespace" markers and
# "for b in bn bn-mcp" lists are not read as invocations.
INVOCATION = re.compile(
    r"(?:^\s*|[|;&(]\s*|\$\(\s*|[{!]\s+|\b(?:then|do|else|time|exec|sudo|env|xargs|watch)\s+)bn\s+(\S.*)"
)


def tokens_of(rest):
    rest = rest.replace("…", " ... ")
    try:
        lex = shlex.shlex(rest, posix=True, punctuation_chars="|&;()")
        lex.whitespace_split = True
        return list(lex)
    except ValueError:
        return rest.split()


def is_placeholder(tok):
    return (
        not tok
        or tok.startswith(("<", "[", "{", "$", "\"", "'", "*"))
        or tok in {"...", "..", "…", "X", "N"}
        or "{{" in tok
        or not re.fullmatch(r"[a-z0-9][a-z0-9-]*", tok)
    )


def check(root, snippet):
    problems = []
    for m in INVOCATION.finditer(snippet):
        toks = tokens_of(m.group(1))
        node, flags = root, set(root.flags)
        descending = True
        for tok in toks:
            if tok in SEPARATORS or tok in {"(", "|&", "--"}:
                break
            if tok.startswith("--"):
                name = tok[2:].split("=", 1)[0]
                if not name or not re.fullmatch(r"[a-z0-9][a-z0-9-]*", name):
                    continue
                if name not in flags and name not in node.flags:
                    # Grouped spellings (`bn list --summary` -> `bn summary`) are argv rewrites that
                    # --help does not list; one that routes answers its own --help.
                    routed = load(root.bin, [*node.path, f"--{name}"], root.cache)
                    if routed.exists:
                        node = routed
                        flags |= routed.flags
                    else:
                        problems.append(f"`bn {' '.join(node.path)}` has no flag --{name}")
                continue
            if tok.startswith("-"):
                continue
            if not descending:
                continue
            if tok not in node.children and node.has_commands and not is_placeholder(tok):
                # Hidden subcommands (`#[command(hide = true)]`) are absent from --help but run.
                hidden = load(root.bin, [*node.path, tok], root.cache)
                if hidden.exists:
                    node.children[tok] = hidden
            if tok in node.children:
                node = node.children[tok]
                flags |= node.flags
                continue
            if is_placeholder(tok):
                descending = False
                continue
            if node.has_commands and not node.positional:
                problems.append(f"`bn {' '.join(node.path)}` has no subcommand '{tok}'")
                break
            descending = False
    return problems


def render_failures(bin_path, path, text):
    """Render the skill with a dummy value for every declared argument; report a failure or a
    `{{placeholder}}` the arguments did not fill."""
    if not text.startswith("---"):
        return []
    front = text.split("---", 2)[1]
    names = re.findall(r"^\s*-\s*\{\s*name:\s*([a-z0-9_]+)", front, re.M)
    name = re.search(r"^name:\s*(\S+)", front, re.M)
    skill = name.group(1) if name else path.rsplit("/", 1)[-1][:-3]
    argv = [bin_path, "skills", "render", skill]
    for arg in names:
        argv += ["--arg", f"{arg}=X"]
    proc = subprocess.run(argv, capture_output=True, text=True)
    if proc.returncode != 0:
        return [f"`bn skills render {skill}` failed: {(proc.stderr or proc.stdout).strip().splitlines()[0]}"]
    left = sorted(set(re.findall(r"\{\{\s*[a-z0-9_]+\s*\}\}", proc.stdout)))
    return [f"`bn skills render {skill}` left {', '.join(left)} unfilled"] if left else []


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/debug/bn")
    ap.add_argument("files", nargs="*")
    args = ap.parse_args()
    files = args.files or sorted(glob.glob("config/prompts/*.md") + glob.glob(".bn/skills/*.md"))
    root = load(args.bin, [], {})
    if not root.children:
        print(f"could not read the command tree from {args.bin} --help", file=sys.stderr)
        return 2
    failed = 0
    render_seen = set()
    for path in files:
        with open(path, encoding="utf-8") as fh:
            text = fh.read()
        for problem in render_failures(args.bin, path, text):
            # Report a render error against the file it names, once (a skill can be reached
            # under more than one path).
            m = re.search(r"(\S+\.md): (.*)", problem)
            where, problem = (m.group(1), m.group(2)) if m else (path, problem)
            if (where, problem) in render_seen:
                continue
            render_seen.add((where, problem))
            print(f"{where}:1: {problem}")
            failed += 1
        seen = set()
        for lineno, snippet in code_snippets(text):
            for problem in check(root, snippet):
                if (lineno, problem) in seen:
                    continue
                seen.add((lineno, problem))
                print(f"{path}:{lineno}: {problem}")
                failed += 1
    if failed:
        print(f"\n{failed} skill problem(s)", file=sys.stderr)
        return 1
    print(f"skill commands: ok ({len(files)} files)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
