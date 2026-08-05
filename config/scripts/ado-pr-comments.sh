#!/usr/bin/env bash
# ado-pr-comments.sh — fetch **human reviewer comments** from the N most-recent COMPLETED pull
# requests of an Azure DevOps repo, for mining a review specialist's standards (see the
# `mine-review-standards` skill). READ-ONLY against ADO.
#
# Output: a JSON array of {pr, title, author, content} on stdout; diagnostics on stderr.
# Degrades LOUDLY: missing `az`, not logged in, unresolved org/project, or an unknown repo each
# exit non-zero with a one-line reason on stderr, so the caller can fall back rather than invent
# standards from nothing.
#
# Usage:
#   ado-pr-comments.sh --source <ado-repo> [--org <org|url>] [--project <project>] [--max N]
#
# `--max` (alias `--top`) caps how many recent COMPLETED PRs to scan (default 200); the listing
# paginates so you can mine as many as the repo has, to seed a solid starting corpus. Org/project
# default to your `az devops configure` defaults when the flags are omitted. This tool is strictly
# READ-ONLY (only GET-style `az` reads) — it never posts, updates, votes, or deletes anything in
# Azure DevOps.
set -u

MAX=200                 # total completed PRs to scan (raise for a bigger corpus)
PAGE="${ADO_PAGE:-100}" # per-request page size for listing (override for tests)
JOBS="${ADO_JOBS:-6}"   # concurrent thread fetches (bounded parallelism; override for tests)
ORG=""
PROJECT=""
REPO=""

while [ $# -gt 0 ]; do
  case "$1" in
    --source | --repo) REPO="${2:-}"; shift 2 ;;
    --org) ORG="${2:-}"; shift 2 ;;
    --project) PROJECT="${2:-}"; shift 2 ;;
    --max | --top) MAX="${2:-}"; shift 2 ;;
    -h | --help) sed -n '2,18p' "$0"; exit 0 ;;
    *) echo "ado-pr-comments: unknown arg '$1'" >&2; exit 2 ;;
  esac
done

[ -n "$REPO" ] || { echo "ado-pr-comments: --source <ado-repo> is required" >&2; exit 2; }
command -v az >/dev/null 2>&1 || { echo "ado-pr-comments: 'az' CLI not found on PATH" >&2; exit 3; }
command -v python3 >/dev/null 2>&1 || { echo "ado-pr-comments: 'python3' not found on PATH" >&2; exit 3; }

# Resolve org/project from flags, else the az devops defaults. `az` may be the Windows CLI under
# WSL, whose output carries CRLF — strip CRs so the URL/project don't gain a trailing \r.
defaults="$(az devops configure --list 2>/dev/null | tr -d '\r' || true)"
[ -n "$ORG" ] || ORG="$(printf '%s\n' "$defaults" | sed -n 's/^organization = //p' | head -1)"
[ -n "$PROJECT" ] || PROJECT="$(printf '%s\n' "$defaults" | sed -n 's/^project = //p' | head -1)"
ORG="$(printf '%s' "$ORG" | tr -d '\r' | tr -d '[:space:]')"
PROJECT="$(printf '%s' "$PROJECT" | tr -d '\r' | sed 's/[[:space:]]*$//')"
MAX="$(printf '%s' "$MAX" | tr -d '\r[:space:]')"
PAGE="$(printf '%s' "$PAGE" | tr -d '\r[:space:]')"
JOBS="$(printf '%s' "$JOBS" | tr -d '\r[:space:]')"
case "$MAX" in ''|*[!0-9]*) MAX=200 ;; esac
case "$PAGE" in ''|*[!0-9]*|0) PAGE=100 ;; esac
case "$JOBS" in ''|*[!0-9]*|0) JOBS=6 ;; esac
[ -n "$ORG" ] || { echo "ado-pr-comments: no organization (pass --org or set 'az devops configure')" >&2; exit 4; }
[ -n "$PROJECT" ] || { echo "ado-pr-comments: no project (pass --project or set 'az devops configure')" >&2; exit 4; }

# Normalize org to a base URL.
case "$ORG" in
  http*) ORG_URL="${ORG%/}" ;;
  *) ORG_URL="https://dev.azure.com/$ORG" ;;
esac

# Azure DevOps resource id — scopes the `az rest` token to ADO so the threads API is reachable.
ADO_APP="499b84ac-1321-427f-aa17-267ca6975798"

# List completed PRs (id + title), paginating with --skip so we can mine as many as the repo has
# up to MAX. READ-ONLY: `az repos pr list` only reads.
rows=()
skip=0
while [ "${#rows[@]}" -lt "$MAX" ]; do
  want=$(( MAX - ${#rows[@]} ))
  [ "$want" -gt "$PAGE" ] && want="$PAGE"
  page="$(az repos pr list \
    --organization "$ORG_URL" --project "$PROJECT" --repository "$REPO" \
    --status completed --top "$want" --skip "$skip" -o json 2>/dev/null)" || {
    echo "ado-pr-comments: failed to list completed PRs for '$REPO' in $ORG_URL/$PROJECT" >&2
    exit 5
  }
  mapfile -t page_rows < <(printf '%s' "$page" | python3 -c '
import sys, json
try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(0)
for p in data:
    print("%s\t%s" % (p.get("pullRequestId", ""), (p.get("title", "") or "").replace("\t", " ")))
' 2>/dev/null)
  got=${#page_rows[@]}
  [ "$got" -eq 0 ] && break
  rows+=("${page_rows[@]}")
  skip=$(( skip + got ))
  echo "ado-pr-comments: listed ${#rows[@]} completed PRs…" >&2
  [ "$got" -lt "$want" ] && break   # short page => no more results
done

if [ "${#rows[@]}" -eq 0 ]; then
  echo "ado-pr-comments: no completed PRs found for '$REPO'" >&2
  echo "[]"
  exit 0
fi

fetch_threads() {
  az rest --method get --resource "$ADO_APP" \
    --url "$ORG_URL/$PROJECT/_apis/git/repositories/$REPO/pullRequests/$1/threads?api-version=7.1" \
    -o json 2>/dev/null
}

# Save each PR's raw threads JSON to a temp dir, then do ONE guarded pass. Writing per-PR files
# (instead of spawning a python per PR) is faster and keeps a single malformed response — e.g. a
# thread payload with `"value": null` — from aborting the whole run.
workdir="$(mktemp -d "${TMPDIR:-/tmp}/ado-pr-comments.XXXXXX")" || {
  echo "ado-pr-comments: could not create a temp dir" >&2; exit 6; }
trap 'rm -rf "$workdir"' EXIT
manifest="$workdir/manifest.tsv"
: > "$manifest"

# Fetch each PR's threads into its own temp file, up to JOBS at a time. Bounded parallelism keeps
# the many slow per-call `az` round-trips (each ~seconds under WSL) from serializing, so a large
# --max stays practical. Uses bash's `wait -n` (4.3+; WSL ships 5.x) to cap concurrency.
n=0
inflight=0
total=${#rows[@]}
for row in "${rows[@]}"; do
  id="${row%%$'\t'*}"
  title="${row#*$'\t'}"
  [ -n "$id" ] || continue
  n=$(( n + 1 ))
  printf '%s\t%s\n' "$id" "$title" >> "$manifest"
  {
    t="$(fetch_threads "$id")"
    [ -n "$t" ] && printf '%s' "$t" > "$workdir/$id.json"
  } &
  inflight=$(( inflight + 1 ))
  if [ "$inflight" -ge "$JOBS" ]; then
    wait -n 2>/dev/null || true
    inflight=$(( inflight - 1 ))
  fi
  [ $(( n % 25 )) -eq 0 ] && echo "ado-pr-comments: dispatched $n/$total PR fetches…" >&2
done
wait
echo "ado-pr-comments: fetched all $total PR thread sets ($JOBS-way)" >&2

# One guarded pass over the saved threads: extract genuine human "text" comments.
WORKDIR="$workdir" python3 -c '
import os, sys, json, re
workdir = os.environ["WORKDIR"]
manifest = os.path.join(workdir, "manifest.tsv")
# Bot/service/agent author filter — the corpus must be REAL PEOPLE only. Matches service
# accounts (build/deploy/policy bots), named review bots incl. camelCase "…Bot", and coding
# agents (Copilot / Agency / autopilot). Validated to drop only non-human accounts and keep
# every real reviewer across the WebApp + BigMono corpora.
BOT = re.compile(
    r"(\[bot\]|bot@|\bbot\b|bot$|[a-z]bot\b"
    r"|automation|autopilot|azure ?pipelines|policy service|codeflow|servicehooks"
    r"|service account|vsts|nitpicker|gitops|core platform|build.?account"
    r"|\bagency\b|agency[- ]?access|copilot|coding.?agent|\bagent\b)",
    re.I)
# Automated-reviewer headers: a leading bold agent/gate name; a bracketed tag banner such as
# "**[React Warning]**", "[Scope Blocking]", "[copilot] MINOR …"; or a markdown "## … Review
# Summary"/"## … Security Review" heading — none of which a human opens a review comment with.
BANNER = re.compile(
    r"^\s*(<[^>]+>\s*)?(\*\*)?\[[^\]]{1,60}\]"
    r"|^\s*(<[^>]+>\s*)?\*\*[^*]{0,60}\b(agent|bot|gate|policy|guard|nitpicker|review\s*complete)\b"
    r"|^\s*(<[^>]+>\s*)?(#{1,6}\s|\*\*)\s*[^*\n]{0,60}\b(review summary|security review|performance (re-?review|check)|ai code review|best practices review|reconciled)\b"
    r"|^\s*(<[^>]+>\s*)?(#{1,6}\s|\*\*)?\s*(e2e best practices review|automated (pre-?pr )?review|/?perf-review)\b",
    re.I)
# Automated-comment body signals — boilerplate no human writes by hand, incl. Copilot coding-agent
# session banners and pipeline/gate trailers.
CONTENT_BOT = re.compile(
    r"(this comment triggered|triggered based on this|nitpicker comment|auto-?generated"
    r"|automatically generated|this is an automated|submitted via [\x22\x27]|task duration:"
    r"|link to sessions ui|pas performance check|gitopsuseragent|pr-monk-reviewed|privacy reviews wiki"
    r"|comment-only (verdict|voting)|/perf-review|pr-review --|reviewed \d+ (test )?files? against)",
    re.I)
TAG = re.compile(r"</?(?:b|i|u|a|br|p|div|span|code|pre|strong|em|ul|ol|li|img|h[1-6]|table|thead|tbody|tr|td|th|blockquote|hr|sub|sup|font)\b[^>]*>", re.I)  # strip real HTML, keep code generics like List<String>
out = []
npr = 0
try:
    mf = open(manifest, encoding="utf-8")
except Exception:
    mf = []
for line in mf:
    line = line.rstrip("\n")
    if not line:
        continue
    pr, _, title = line.partition("\t")
    npr += 1
    try:
        with open(os.path.join(workdir, pr + ".json"), encoding="utf-8") as f:
            data = json.load(f)
    except Exception:
        continue
    if not isinstance(data, dict):
        continue
    for th in (data.get("value") or []):
        if not isinstance(th, dict):
            continue
        for c in (th.get("comments") or []):
            if not isinstance(c, dict):
                continue
            if c.get("commentType") != "text":  # drop system/status/code-change threads
                continue
            author = ((c.get("author") or {}).get("displayName") or "").strip()
            uniq = ((c.get("author") or {}).get("uniqueName") or "").strip()
            content = (c.get("content") or "").strip()
            content = re.sub(r"<!--.*?-->", "", content, flags=re.S).strip()  # drop HTML-comment annotations
            if not content:
                continue
            if BOT.search(author) or BOT.search(uniq):
                continue
            if BANNER.search(content):        # automated-review banner
                continue
            if CONTENT_BOT.search(content):   # automated boilerplate body
                continue
            content = TAG.sub("", content).strip()
            if not content:
                continue
            if len(content) > 1200:
                content = content[:1200] + "…"
            out.append({"pr": pr, "title": title, "author": author, "content": content})
print(json.dumps(out, indent=2, ensure_ascii=False))
sys.stderr.write("ado-pr-comments: collected %d human comments from %d PRs\n" % (len(out), npr))
'
