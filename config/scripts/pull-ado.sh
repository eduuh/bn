#!/usr/bin/env bash
# pull-ado.sh — pull assigned, open Azure DevOps work items into a bn repo's main note as
# idempotent "[#<id>] <title>" todos (the deterministic body of the `pull-ado` skill).
# PULL-ONLY: reads ADO, writes only to bn. Never writes back to ADO.
#
# Usage:
#   pull-ado.sh [--repo <bn-repo>] [--iteration <path|work-item-id|url>]
#
# --repo defaults to the current git repo's basename. --iteration filters to one iteration
# path; a numeric work-item id or URL is resolved to that item's System.IterationPath first.
# Omitted → all assigned open items. Closed/Resolved/Removed/Done are filtered by the query;
# `--if-absent "[#<id>]"` keeps re-pulls (and completed todos) from being resurrected.
#
# Degrades LOUDLY: missing az/python3 or an unresolvable repo exits non-zero with a one-line
# reason on stderr, so the caller reports the failure instead of inventing items.
set -u

BN="${BN:-bn}"
REPO=""
ITER=""

while [ $# -gt 0 ]; do
  case "$1" in
    --repo) REPO="${2:-}"; shift 2 ;;
    --iteration) ITER="${2:-}"; shift 2 ;;
    -h | --help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "pull-ado: unknown arg '$1'" >&2; exit 2 ;;
  esac
done

command -v az >/dev/null 2>&1 || { echo "pull-ado: 'az' CLI not found on PATH" >&2; exit 3; }
command -v python3 >/dev/null 2>&1 || { echo "pull-ado: 'python3' not found on PATH" >&2; exit 3; }

if [ -z "$REPO" ]; then
  top="$(git rev-parse --show-toplevel 2>/dev/null)" ||
    { echo "pull-ado: not in a git repo — pass --repo <bn-repo>" >&2; exit 2; }
  REPO="$(basename "$top")"
fi

# Resolve --iteration: a numeric id (or a URL ending in one) → that item's iteration path;
# anything else is taken as a literal iteration path.
ITER_CLAUSE=""
if [ -n "$ITER" ]; then
  id=""
  case "$ITER" in
    *[!0-9]*) case "$ITER" in http*) id="${ITER##*/}" ;; esac ;;
    *) id="$ITER" ;;
  esac
  case "$id" in
    '' | *[!0-9]*) : ;; # not an id — keep $ITER as the literal path
    *)
      ITER="$(az boards work-item show --id "$id" -o json 2>/dev/null |
        python3 -c 'import sys,json; print(json.loads(sys.stdin.read(), strict=False)["fields"]["System.IterationPath"])')" ||
        { echo "pull-ado: could not resolve iteration from work item $id" >&2; exit 4; }
      ;;
  esac
  esc="$(printf '%s' "$ITER" | sed "s/'/''/g")"
  ITER_CLAUSE=" AND [System.IterationPath] = '$esc'"
fi

WIQL="SELECT [System.Id],[System.Title],[System.State],[System.WorkItemType] FROM WorkItems WHERE [System.AssignedTo] = @Me AND [System.State] NOT IN ('Closed','Resolved','Removed','Done')$ITER_CLAUSE ORDER BY [System.State]"

# ADO JSON may carry control characters, hence strict=False.
items="$(az boards query --wiql "$WIQL" -o json | python3 -c '
import sys, json
rows = json.loads(sys.stdin.read(), strict=False)
for w in rows:
    f = w.get("fields", {})
    i = f.get("System.Id") or w.get("id")
    t = str(f.get("System.Title") or "").replace("\t", " ").replace("\n", " ").strip()
    if i and t:
        print(f"{i}\t{t}")
')" || { echo "pull-ado: az boards query failed" >&2; exit 4; }

if [ -z "$items" ]; then
  echo "pull-ado: no assigned open work items — $REPO/main unchanged"
  exit 0
fi

new=0
skipped=0
new_ids=""
while IFS=$'\t' read -r id title; do
  [ -n "$id" ] || continue
  out="$("$BN" add --repo "$REPO" --branch main todo "[#$id] $title" --if-absent "[#$id]")" ||
    { echo "pull-ado: bn add failed for #$id" >&2; exit 5; }
  case "$out" in
    "Already in"*) skipped=$((skipped + 1)) ;;
    *) new=$((new + 1)); new_ids="$new_ids #$id" ;;
  esac
done <<EOF
$items
EOF

echo "pull-ado: $((new + skipped)) assigned items → $REPO/main — $new new, $skipped already present"
[ -z "$new_ids" ] || echo "new:$new_ids"
