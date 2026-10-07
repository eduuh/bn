#!/usr/bin/env sh
# The 1000-line "never grow" ratchet. See .file-size-baseline for the rules it enforces.
#
#   scripts/check-file-size.sh            full check   (CI `gates` job, pre-push, bn verify)
#   scripts/check-file-size.sh --staged   staged blobs (pre-commit; sub-second)
#   scripts/check-file-size.sh --update   rewrite the baseline from the working tree
#   scripts/check-file-size.sh --list     print every in-scope file over the limit, biggest first
#
# POSIX sh + awk on purpose, not Python. This runs in the pre-commit hook (where an interpreter
# start on every commit is felt), inside `bn verify`'s disposable rebased worktree (spawned via
# `/bin/sh -c` with a hand-built PATH that a heavier dependency could miss), and on three Fedora
# boxes plus a mac where `sh` and `awk` are the only interpreters guaranteed present.
set -eu

LIMIT="${FILE_SIZE_LIMIT:-1000}"
ROOT="$(git rev-parse --show-toplevel)"
BASELINE="$ROOT/.file-size-baseline"
MODE="${1:-check}"
cd "$ROOT"

# Scope: TRACKED Rust and TypeScript sources, minus generated `.d.ts`.
#
# `git ls-files` buys every exclusion for free — target/, node_modules/ and dist/ are gitignored,
# so they never appear — and the extension list excludes Cargo.lock, package-lock.json and the
# generated design HTML without naming them.
#
# CSS and generated HTML are out of scope.
SCOPE="*.rs *.ts *.tsx"

scope() {
  # `git ls-files` prints a conflicted path once per index stage (base/ours/theirs), so during an
  # unresolved merge every conflicted file is listed two or three times. Unchecked, `--update` then
  # writes that many baseline entries for it — which is exactly how the duplicates #1143 had to clean
  # up got there. `sort -u` makes the listing idempotent whatever the index looks like.
  #
  # `set -f` matters as much as the quoting below it. SCOPE holds *git pathspecs*, and git must
  # receive them literally — but unquoted expansion lets the shell glob them against the working
  # directory first. A single stray `test.ts` sitting in the repo root is enough: the shell rewrites
  # `*.ts` to `test.ts`, git is then asked about that one file, and every real TypeScript source
  # silently leaves the gate's scope. That is not hypothetical — it is how a 1253-line `types.ts`
  # came to look untracked to a local run while CI, whose checkout has no stray, still saw it.
  # A gate that can be switched off by an accidental file in the wrong place is worse than no gate,
  # because it keeps reporting success.
  set -f
  # shellcheck disable=SC2086  # SCOPE is an intentional list of pathspecs, now protected from globbing.
  git ls-files -- $SCOPE | grep -v '\.d\.ts$' | sort -u || true
  set +f
}

# "<path>\t<lines>" for the working tree. One wc(1) for the whole set.
current() {
  scope | tr '\n' '\0' | xargs -0 wc -l 2>/dev/null |
    awk '$2 != "total" && NF >= 2 { n = $1; $1 = ""; sub(/^ +/, ""); print $0 "\t" n }'
}

# "<path>\t<lines>" for the STAGED blobs of staged in-scope files. Reads the index rather than
# the worktree, so the hook judges exactly what you are about to commit.
staged_current() {
  git diff --cached --name-only --diff-filter=ACMR -- $SCOPE 2>/dev/null |
    grep -v '\.d\.ts$' |
    while IFS= read -r f; do
      printf '%s\t%s\n' "$f" "$(git show ":$f" | wc -l | tr -d ' ')"
    done
}

# Everything above the limit, as "<lines>\t<path>" sorted by PATH. Sorted by path, not size, so a
# baseline diff shows only what actually moved instead of reshuffling on every change.
oversize_by_path() {
  current | awk -F'\t' -v lim="$LIMIT" '$2 + 0 > lim { print $2 "\t" $1 }' | sort -k2
}

case "$MODE" in
  --update)
    tmp="$(mktemp)"
    trap 'rm -f "$tmp"' EXIT
    # Preserve the header verbatim; regenerate every entry.
    sed -n '/^#/p' "$BASELINE" 2>/dev/null > "$tmp" || true
    oversize_by_path >> "$tmp"
    mv "$tmp" "$BASELINE"
    trap - EXIT
    echo "rewrote .file-size-baseline ($(grep -cv '^#' "$BASELINE") files over $LIMIT lines)"
    exit 0
    ;;
  --list)
    current | awk -F'\t' -v lim="$LIMIT" '$2 + 0 > lim { print $2 "\t" $1 }' | sort -rn
    exit 0
    ;;
  check | --check | --staged) ;;
  *)
    echo "usage: $0 [--staged|--update|--list]" >&2
    exit 2
    ;;
esac

base="$(mktemp)"
cur="$(mktemp)"
trap 'rm -f "$base" "$cur"' EXIT

grep -v '^#' "$BASELINE" 2>/dev/null | awk 'NF == 2 { print $2 "\t" $1 }' | sort > "$base"
if [ "$MODE" = "--staged" ]; then staged_current > "$cur"; else current > "$cur"; fi

awk -F'\t' -v lim="$LIMIT" -v staged="$([ "$MODE" = "--staged" ] && echo 1 || echo 0)" '
  NR == FNR { base[$1] = $2 + 0; next }
  { cur[$1] = $2 + 0 }
  END {
    fail = 0
    for (f in cur) {
      n = cur[f]
      if (f in base) {
        b = base[f]
        if (n > b) {
          printf "GREW      %s: %d lines > baseline %d — split it, do not grow it\n", f, n, b
          fail = 1
        } else if (n <= lim) {
          printf "GRADUATED %s: %d <= %d — drop it from .file-size-baseline (--update)\n", f, n, lim
          fail = 1
        } else if (n < b) {
          printf "SHRANK    %s: %d < baseline %d — run --update to lock the win in\n", f, n, b
          fail = 1
        }
      } else if (n > lim) {
        printf "OVERSIZE  %s: %d lines > %d — new files may not exceed the limit\n", f, n, lim
        fail = 1
      }
    }
    # A staged view sees only the files in this commit, so it cannot tell a deleted file from an
    # untouched one. Leave STALE to the full check.
    if (!staged) {
      for (f in base) {
        if (!(f in cur)) {
          printf "STALE     %s: baselined but no longer tracked — drop it (--update)\n", f
          fail = 1
        }
      }
    }
    if (fail) {
      print ""
      print "file-size ratchet FAILED. The rules, and how to regenerate, are in .file-size-baseline."
    }
    exit fail
  }
' "$base" "$cur"
