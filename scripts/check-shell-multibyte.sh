#!/bin/sh
# Reject `$name` written directly against a non-ASCII character, e.g. `echo "for $target…"`.
#
# Why this is a real failure and not a style nit: bash decides where a variable name ends with
# isalnum(), which is locale-dependent. On macOS in a UTF-8 locale the C library still classifies
# single bytes 0x80-0xFF as Latin-1 letters, so the first byte of `…` (0xE2, 'â') is read as part
# of the name — bash looks up `target\xe2`, and under `set -euo pipefail` the script dies with
#   install.sh: line 132: target?: unbound variable
# before it does anything. glibc classifies those bytes as non-letters, so the same line runs
# fine on Linux; CI is Linux-only, so nothing here catches it. This did brick install.sh on every
# Mac (#1265). Both /bin/bash 3.2 and Homebrew bash 5.3 fail identically — it is libc, not bash.
#
# The fix is always to brace the expansion: "${target}…". `}` ends the name explicitly, so the
# locale never gets a say.
set -eu

hits=$(
  git ls-files -z |
    LC_ALL=C xargs -0 perl -ne '
      print "$ARGV:$.: $_" if /\$[A-Za-z_][A-Za-z0-9_]*[\x80-\xff]/;
      close ARGV if eof;
    ' 2>/dev/null || true
)

if [ -n "$hits" ]; then
  printf '%s\n%s\n' \
    "Unbraced \$name written against a non-ASCII character — brace it as \${name} (see this script's header):" \
    "$hits" >&2
  exit 1
fi

echo "shell multibyte adjacency: PASS"
