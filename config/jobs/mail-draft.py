#!/usr/bin/env python3
"""mail-draft.py — put a rendered briefing card into Gmail as a **draft**, never a sent mail.

Reads the message on stdin: the first line is the subject, the rest is the Markdown body. Builds an
RFC-5322 message and IMAP-APPENDs it to the Drafts mailbox with the `\\Draft` flag.

Why a draft and not SMTP: a draft is reviewable. The brief is model-authored from a machine-gathered
packet, so the first weeks of it should be read with suspicion — a draft sits in Gmail until *you*
decide it was worth sending. SMTP can only send, so it cannot express that.

Why IMAP and not the Gmail API: APPEND is the whole feature. The API path costs an OAuth client, a
consent screen and refresh-token handling before the first brief ever lands; this needs one app
password and Python's standard library.

Credentials are read from the macOS keychain at run time and never touched by bn's config:

    security add-generic-password -s bn-gmail -a you@gmail.com -w '<16-char app password>'

Requires 2FA on the account (app passwords do not exist without it). Nothing is written to disk and
nothing is logged: the password lives in a local variable and dies with the process.

Every failure path is non-fatal and exits 0 — the card has already landed in `bn staff feed`, and a
missing keychain entry or a flaky network must never fail the morning's cron run.

    bn staff mail standup | config/jobs/mail-draft.py
"""

import email.utils
import imaplib
import os
import subprocess
import sys
import time
from email.message import EmailMessage

# The keychain service holding the Gmail app password. The account name on that entry is the
# address we authenticate as and draft to, so one entry configures the whole thing.
KEYCHAIN_SERVICE = os.environ.get("BN_GMAIL_KEYCHAIN_SERVICE", "bn-gmail")
IMAP_HOST = os.environ.get("BN_GMAIL_IMAP_HOST", "imap.gmail.com")

# Gmail localizes its special folders, so `[Gmail]/Drafts` is not portable across account languages.
# The reliable way is the `\Drafts` special-use attribute advertised by LIST; these are only the
# fallbacks for servers that do not advertise it.
DRAFTS_FALLBACKS = ("[Gmail]/Drafts", "[Google Mail]/Drafts", "Drafts")


# Printed to stdout, and only ever when a draft really landed in the mailbox. Every failure here is
# deliberately non-fatal (exit 0), so the exit code cannot carry this: without a positive signal the
# caller would mark the card read after a skipped delivery, and the brief would be silently consumed
# without ever reaching a human — the precise failure this whole feature exists to undo.
DRAFTED = "drafted"


def log(msg):
    """Diagnostics go to stderr; stdout carries only the DRAFTED signal."""
    print(f"mail-draft: {msg}", file=sys.stderr)


def keychain(service, extra=()):
    """One keychain lookup. Returns None when the entry is absent — never raises, never logs the
    secret."""
    try:
        out = subprocess.run(
            ["security", "find-generic-password", "-s", service, *extra, "-w"],
            capture_output=True,
            text=True,
            timeout=15,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return out.stdout.strip() or None if out.returncode == 0 else None


def keychain_account(service):
    """The account (email address) stored on the keychain entry, parsed from its attribute dump."""
    try:
        out = subprocess.run(
            ["security", "find-generic-password", "-s", service],
            capture_output=True,
            text=True,
            timeout=15,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if out.returncode != 0:
        return None
    # The dump prints `"acct"<blob>="you@gmail.com"` among the attributes.
    for line in out.stdout.splitlines():
        line = line.strip()
        if line.startswith('"acct"'):
            _, _, value = line.partition("=")
            return value.strip().strip('"') or None
    return None


def drafts_mailbox(imap):
    """The Drafts mailbox, preferring the server's `\\Drafts` special-use flag over a guessed name."""
    try:
        ok, boxes = imap.list()
        if ok == "OK":
            for raw in boxes:
                line = raw.decode(errors="replace") if isinstance(raw, bytes) else str(raw)
                if "\\Drafts" in line:
                    # `(\HasNoChildren \Drafts) "/" "[Gmail]/Drafts"` — the name is the last quoted run.
                    parts = line.split('"')
                    if len(parts) >= 2 and parts[-2]:
                        return parts[-2]
    except imaplib.IMAP4.error:
        pass
    return None


def build_message(sender, subject, body):
    """An RFC-5322 message carrying the brief as plain text (Markdown reads fine unrendered)."""
    msg = EmailMessage()
    msg["From"] = sender
    msg["To"] = sender  # a note to self: the brief has exactly one reader
    msg["Subject"] = subject
    msg["Date"] = email.utils.formatdate(localtime=True)
    msg["Message-ID"] = email.utils.make_msgid(domain="bn.local")
    msg.set_content(body)
    return msg


def main():
    raw = sys.stdin.read()
    if not raw.strip():
        log("nothing on stdin — no draft written")
        return 0

    subject, _, body = raw.partition("\n")
    subject = subject.strip() or "bn brief"
    if not body.strip():
        log("no body — no draft written")
        return 0

    password = keychain(KEYCHAIN_SERVICE)
    if not password:
        log(
            f"no '{KEYCHAIN_SERVICE}' entry in the keychain — skipping the draft. "
            f"Add one with: security add-generic-password -s {KEYCHAIN_SERVICE} "
            "-a you@gmail.com -w '<app password>'"
        )
        return 0

    sender = os.environ.get("BN_GMAIL_ADDRESS") or keychain_account(KEYCHAIN_SERVICE)
    if not sender:
        log(f"the '{KEYCHAIN_SERVICE}' keychain entry has no account (-a) — skipping the draft")
        return 0

    msg = build_message(sender, subject, body)

    try:
        with imaplib.IMAP4_SSL(IMAP_HOST, timeout=30) as imap:
            imap.login(sender, password)
            box = drafts_mailbox(imap)
            candidates = [box] if box else list(DRAFTS_FALLBACKS)
            for name in candidates:
                # IMAP mailbox names with spaces must be quoted in the APPEND command.
                quoted = f'"{name}"' if " " in name else name
                ok, _ = imap.append(
                    quoted, "\\Draft", imaplib.Time2Internaldate(time.time()), msg.as_bytes()
                )
                if ok == "OK":
                    log(f"draft written to {name} — {subject}")
                    print(DRAFTED)
                    return 0
            log(f"could not append to any Drafts mailbox (tried {', '.join(candidates)})")
            return 0
    except imaplib.IMAP4.error as e:
        # Most often a bad/revoked app password, or IMAP disabled on the account.
        log(f"IMAP refused the draft ({e}) — check the app password and that IMAP is enabled")
        return 0
    except (OSError, TimeoutError) as e:
        log(f"could not reach {IMAP_HOST} ({e}) — the card is still in `bn staff feed`")
        return 0


if __name__ == "__main__":
    sys.exit(main())
