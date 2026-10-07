#!/usr/bin/env python3
"""Prepare the next release: bump the shared workspace version and prepend a CHANGELOG section.

This replaces release-plz's `release-pr` command.

release-plz determines the next version by running `cargo package` on the tree at the last released
tag, and this workspace can never be packaged: `bn-core`/`bn-cli` `include_str!` repo-root files
(`docs/config.md`, `config/skills/staff/*.md`, `config/workflow/tmux.conf`,
`config/prompts/quarter-review.md`) that by definition cannot live inside a crate tarball. That is
deliberate — bn is a single-repo private tool, never published to a registry (`release-plz.toml:
git_only = true`) — so the packaging requirement is one this repo will never satisfy. Every
`release-plz PR` run failed for exactly this reason from the
v0.2.0 tag onward: that tag's manifest predates the explicit dependency versions added in #983, and
a tag is immutable, so the fix could never reach the tree release-plz actually packaged.

The *other* half of release-plz is untouched. `release-plz release` still pushes the single
`v{version}` tag once a release PR merges, and cargo-dist still builds the GitHub Release from that
tag. It never runs `cargo package`, so it was never affected.

Local and side-effect-free by design: this edits the working tree and prints the new version to
stdout. Committing, pushing and opening the PR is the workflow's job, so this stays runnable by
hand to preview exactly what the next release would be:

    python3 scripts/release-pr.py            # apply the bump to the working tree
    python3 scripts/release-pr.py --dry-run  # print the version + changelog, change nothing

Prints nothing and exits 0 when there is nothing releasable, so the caller can no-op quietly.
"""

from __future__ import annotations

import argparse
import datetime
import re
import subprocess
import sys
from pathlib import Path

# Commit types that constitute a release. A window of pure chore/docs/ci/test commits produces no
# release, matching release-plz: a tag should mean something a user could notice actually changed.
RELEASABLE = ("feat", "fix", "perf", "refactor", "revert")

# Changelog sections, in render order.
SECTIONS = [
    ("feat", "Features"),
    ("fix", "Bug Fixes"),
    ("perf", "Performance"),
    ("refactor", "Refactor"),
    ("revert", "Reverts"),
]

# `type(scope)!: subject` — the conventional-commit shape, with scope and `!` both optional.
CONVENTIONAL = re.compile(r"^(?P<type>[a-z]+)(?:\((?P<scope>[^)]*)\))?(?P<bang>!)?:\s*(?P<subject>.+)$")

# A trailing `(#123)` that GitHub appends on squash-merge; the changelog links it separately.
PR_SUFFIX = re.compile(r"\s*\(#(\d+)\)\s*$")


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], check=True, capture_output=True, text=True
    ).stdout.strip()


def last_tag() -> str | None:
    tags = git("tag", "-l", "v*", "--sort=-v:refname").splitlines()
    return tags[0] if tags else None


def commits(rev_range: str) -> list[tuple[str, str]]:
    """(subject, body) for every non-merge commit in `rev_range`."""
    raw = subprocess.run(
        ["git", "log", "--no-merges", "--format=%s%x00%b%x1e", rev_range],
        check=True, capture_output=True, text=True,
    ).stdout
    out = []
    for record in raw.split("\x1e"):
        record = record.strip("\n")
        if not record:
            continue
        subject, _, body = record.partition("\x00")
        out.append((subject, body))
    return out


def classify(subject: str) -> tuple[str | None, str | None, bool, str]:
    """(type, scope, breaking, subject-without-prefix)."""
    m = CONVENTIONAL.match(subject)
    if not m:
        return None, None, False, subject
    return m["type"], m["scope"], bool(m["bang"]), m["subject"]


def next_version(current: str, has_feat: bool, breaking: bool) -> str:
    major, minor, patch = (int(p) for p in current.split("."))
    # Pre-1.0 semver: a breaking change bumps the minor, never the major. bn has never promised a
    # stable API, and silently going 1.0 on the first `feat!:` would overstate its maturity.
    if breaking or has_feat:
        return f"{major}.{minor + 1}.0"
    return f"{major}.{minor}.{patch + 1}"


def render_changelog(version: str, entries: list[tuple[str, str | None, bool, str, str]]) -> str:
    """`entries` are (type, scope, breaking, subject, pr) tuples."""
    today = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d")
    out = [f"## [{version}] - {today}"]

    breaking = [e for e in entries if e[2]]
    if breaking:
        out.append("\n### ⚠ Breaking Changes\n")
        out.extend(_line(e) for e in breaking)

    for kind, heading in SECTIONS:
        rows = [e for e in entries if e[0] == kind and not e[2]]
        if not rows:
            continue
        out.append(f"\n### {heading}\n")
        out.extend(_line(e) for e in rows)
    return "\n".join(out) + "\n"


def _line(entry: tuple[str, str | None, bool, str, str]) -> str:
    _kind, scope, _breaking, subject, pr = entry
    text = f"**{scope}**: {subject}" if scope else subject
    return f"- {text}" + (f" (#{pr})" if pr else "")


def bump_manifest(path: Path, current: str, new: str) -> None:
    src = path.read_text()

    # The shared [workspace.package] version. Every crate inherits it via `version.workspace = true`,
    # which is what keeps the workspace in lockstep under a single tag.
    pattern = re.compile(
        r"(\[workspace\.package\](?:[^\[]*?)\bversion\s*=\s*\")" + re.escape(current) + r"(\")",
        re.S,
    )
    src, n = pattern.subn(r"\g<1>" + new + r"\g<2>", src, count=1)
    if n != 1:
        sys.exit(f'release-pr: no [workspace.package] version = "{current}" in {path}')

    # Internal path dependencies pin that version explicitly (added in #983 so cargo has a version
    # requirement to work with), so they must move in lockstep or the workspace stops resolving.
    src = re.sub(
        r"(\b(?:bn-core|bn-mcp)\s*=\s*\{[^}]*\bversion\s*=\s*\")" + re.escape(current) + r"(\")",
        r"\g<1>" + new + r"\g<2>",
        src,
    )
    path.write_text(src)


HEADER = (
    "# Changelog\n\n"
    "All notable changes to this project are documented here. Generated from conventional commits\n"
    "by `scripts/release-pr.py`; the `v{version}` tag is pushed by `release-plz release` on merge.\n"
)


def prepend_changelog(path: Path, section: str) -> None:
    """Insert `section` above the newest existing release, keeping the file header intact."""
    body = path.read_text() if path.exists() else HEADER
    if not body.lstrip().startswith("# Changelog"):
        body = HEADER + "\n" + body

    lines = body.splitlines(keepends=True)
    # The first `## ` line is the newest release; everything above it is the header block.
    insert_at = next(
        (i for i, line in enumerate(lines) if line.startswith("## ")), len(lines)
    )
    head = "".join(lines[:insert_at]).rstrip("\n")
    rest = "".join(lines[insert_at:]).lstrip("\n")
    path.write_text(head + "\n\n" + section.rstrip("\n") + "\n" + ("\n" + rest if rest else ""))


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--dry-run", action="store_true", help="print what would change, touch nothing")
    args = ap.parse_args()

    root = Path(git("rev-parse", "--show-toplevel"))
    tag = last_tag()
    rev_range = f"{tag}..HEAD" if tag else "HEAD"
    current = tag[1:] if tag else "0.0.0"

    entries: list[tuple[str, str | None, bool, str, str]] = []
    has_feat = breaking_any = False
    for subject, body in commits(rev_range):
        kind, scope, bang, text = classify(subject)
        pr_match = PR_SUFFIX.search(text)
        pr = pr_match.group(1) if pr_match else ""
        text = PR_SUFFIX.sub("", text)
        is_breaking = bang or bool(re.search(r"^BREAKING[ -]CHANGE", body, re.M))
        if kind in RELEASABLE:
            entries.append((kind, scope, is_breaking, text, pr))
            has_feat = has_feat or kind == "feat"
            breaking_any = breaking_any or is_breaking

    if not entries:
        return 0

    new = next_version(current, has_feat, breaking_any)
    section = render_changelog(new, entries)

    if args.dry_run:
        print(f"{current} -> {new}\n", file=sys.stderr)
        print(section, file=sys.stderr)
        print(new)
        return 0

    bump_manifest(root / "Cargo.toml", current, new)
    prepend_changelog(root / "CHANGELOG.md", section)

    # Refresh the workspace members' versions in Cargo.lock. Offline first: only local versions
    # changed, so a network hiccup must not fail release prep.
    for cmd in (
        ["cargo", "update", "--workspace", "--offline"],
        ["cargo", "update", "--workspace"],
    ):
        if subprocess.run(cmd, cwd=root, capture_output=True).returncode == 0:
            break

    print(new)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
